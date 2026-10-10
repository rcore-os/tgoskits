#!/bin/bash
set -euo pipefail

rootfs=${1:-}
toolchain=${2:-}
source_archive=${3:-}
source_sha256=${4:-}
source_meta=${5:-}
app_root=/opt/starry-orangepi5plus-selfbuild
rootfs_marker="$rootfs/.starry-selfbuild-rootfs"
rootfs_format=1
debian_suite=bookworm

fail() {
    echo "provision-rootfs: $*" >&2
    exit 1
}

is_starry_boot_script() {
    local identity_rc
    set +e
    "$app_root/boot_script_is_starry.sh" "$1"
    identity_rc=$?
    set -e
    case "$identity_rc" in
        0) return 0 ;;
        1) return 1 ;;
        *) fail "cannot inspect U-Boot script: $1" ;;
    esac
}

[ "$(id -u)" = 0 ] || fail "must run as root"
[ "$rootfs" = "$app_root/rootfs" ] || fail "unexpected rootfs path: $rootfs"
case "$toolchain" in
    ''|*[!A-Za-z0-9._-]*) fail "invalid Rust toolchain: $toolchain" ;;
esac
"$app_root/validate_sha256.sh" "$source_sha256" || fail "invalid source SHA-256"
[ -s "$source_archive" ] || fail "source archive is missing: $source_archive"
[ -s "$source_meta" ] || fail "source metadata is missing: $source_meta"
printf '%s  %s\n' "$source_sha256" "$source_archive" | sha256sum -c - >/dev/null

verify_linux_default_boot() {
    local evidence bootcmd current_sha recovery_copy linux_backup
    evidence="$app_root/linux-boot-default.meta"
    install -d -m 0755 "$app_root"
    : > "$evidence"

    linux_backup=/boot/boot.scr.tgoskits-backup
    if [ -f /boot/boot.scr ] && is_starry_boot_script /boot/boot.scr; then
        [ -f "$linux_backup" ] \
            || fail "/boot/boot.scr selects StarryOS and no Linux backup exists"
        if is_starry_boot_script "$linux_backup"; then
            fail "the saved U-Boot script is not a verifiable Linux default"
        fi
        current_sha="$(sha256sum /boot/boot.scr | awk '{ print $1 }')"
        recovery_copy="/boot/boot.scr.starry-before-selfbuild-${current_sha}"
        if [ ! -f "$recovery_copy" ]; then
            install -m 0644 /boot/boot.scr "$recovery_copy"
        fi
        install -m 0644 "$linux_backup" /boot/boot.scr
        sync
        cmp -s /boot/boot.scr "$linux_backup" \
            || fail "failed to restore the verified Linux U-Boot script"
        printf 'restored_linux_boot_script=%s\n' "$linux_backup" >> "$evidence"
        printf 'previous_starry_boot_script=%s\n' "$recovery_copy" >> "$evidence"
        echo "provision-rootfs: restored U-Boot default to Linux; saved $recovery_copy"
    fi

    if command -v fw_printenv >/dev/null 2>&1; then
        bootcmd="$(fw_printenv -n bootcmd 2>/dev/null || true)"
        if [ -n "$bootcmd" ]; then
            printf 'bootcmd=%s\n' "$bootcmd" >> "$evidence"
            case "$bootcmd" in
                *starry*|*image.fit*) fail "U-Boot bootcmd still selects StarryOS: $bootcmd" ;;
                *distro_bootcmd*|*bootflow*|*boot.scr*|*extlinux*)
                    printf 'verified_by=fw_printenv\n' >> "$evidence"
                    return
                    ;;
                *) fail "unknown U-Boot bootcmd; refusing to rewrite it: $bootcmd" ;;
            esac
        fi
    fi

    if [ -f /boot/boot.scr ]; then
        if is_starry_boot_script /boot/boot.scr; then
            fail "/boot/boot.scr appears to select StarryOS"
        fi
        sha256sum /boot/boot.scr >> "$evidence"
        printf 'verified_by=linux_boot_script\n' >> "$evidence"
        return
    fi
    if [ -f /boot/extlinux/extlinux.conf ]; then
        sha256sum /boot/extlinux/extlinux.conf >> "$evidence"
        printf 'verified_by=extlinux\n' >> "$evidence"
        return
    fi
    fail "cannot prove that U-Boot defaults to Linux"
}

mounted_proc=0
mounted_dev=0
mounted_sys=0
cleanup_mounts() {
    if [ "$mounted_sys" = 1 ]; then umount -R "$rootfs/sys" || true; fi
    if [ "$mounted_dev" = 1 ]; then umount -R "$rootfs/dev" || true; fi
    if [ "$mounted_proc" = 1 ]; then umount "$rootfs/proc" || true; fi
}
trap cleanup_mounts EXIT

mount_chroot_filesystems() {
    install -d -m 0755 "$rootfs/proc" "$rootfs/dev" "$rootfs/sys"
    if ! mountpoint -q "$rootfs/proc"; then
        mount --bind /proc "$rootfs/proc"
        mount --make-rslave "$rootfs/proc"
        mounted_proc=1
    fi
    if ! mountpoint -q "$rootfs/dev"; then
        mount --rbind /dev "$rootfs/dev"
        mount --make-rslave "$rootfs/dev"
        mounted_dev=1
    fi
    if ! mountpoint -q "$rootfs/sys"; then
        mount --rbind /sys "$rootfs/sys"
        mount --make-rslave "$rootfs/sys"
        mounted_sys=1
    fi
}

chroot_bash() {
    chroot "$rootfs" /usr/bin/env \
        HOME=/root \
        CARGO_HOME=/root/.cargo \
        PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
        DEBIAN_FRONTEND=noninteractive \
        /bin/bash -lc "$1"
}

ensure_lwprintf_cross_compiler() {
    if ! chroot_bash 'command -v aarch64-linux-musl-gcc >/dev/null 2>&1'; then
        chroot_bash 'command -v aarch64-linux-gnu-gcc >/dev/null 2>&1'
        install -d -m 0755 "$rootfs/usr/local/bin"
        ln -s /usr/bin/aarch64-linux-gnu-gcc \
            "$rootfs/usr/local/bin/aarch64-linux-musl-gcc"
    fi
    chroot_bash 'aarch64-linux-musl-gcc -print-sysroot >/dev/null'
}

rust_toolchain_ready() {
    chroot_bash "rustup run '$toolchain' rustc --version >/dev/null 2>&1 && \
        rustup component list --toolchain '$toolchain' --installed | \
            grep -q '^rust-src' && \
        rustup component list --toolchain '$toolchain' --installed | \
            grep -q '^llvm-tools'"
}

prepare_base_rootfs() {
    if [ -e "$rootfs" ]; then
        fail "incomplete rootfs exists without a valid marker: $rootfs"
    fi

    apt-get update
    DEBIAN_FRONTEND=noninteractive apt-get install -y debootstrap
    install -d -m 0755 "$(dirname "$rootfs")"
    debootstrap --arch=arm64 --variant=minbase \
        --include=ca-certificates,curl,gnupg \
        "$debian_suite" "$rootfs" http://deb.debian.org/debian
    mount_chroot_filesystems
    chroot_bash 'apt-get update && apt-get install -y \
        bash bc binutils build-essential ca-certificates clang cmake coreutils curl file \
        gcc-aarch64-linux-gnu git jq libclang-dev libssl-dev libudev-dev lld llvm-dev \
        linux-perf make ninja-build patch pkg-config procps python3 rsync time u-boot-tools \
        util-linux && apt-get check'
    chroot_bash 'curl --proto "=https" --tlsv1.2 -fsSL \
        https://sh.rustup.rs -o /var/tmp/rustup-init.sh && \
        sh /var/tmp/rustup-init.sh -y --profile minimal --default-toolchain none'

    cat > "$rootfs_marker" <<EOF
rootfs_format=$rootfs_format
debian_suite=$debian_suite
architecture=arm64
libc=glibc
EOF
}

verify_linux_default_boot

if [ -f "$rootfs_marker" ]; then
    grep -qx "rootfs_format=$rootfs_format" "$rootfs_marker" \
        || fail "unsupported rootfs format"
    grep -qx "debian_suite=$debian_suite" "$rootfs_marker" \
        || fail "rootfs Debian suite mismatch"
    grep -qx 'architecture=arm64' "$rootfs_marker" \
        || fail "rootfs architecture mismatch"
    grep -qx 'libc=glibc' "$rootfs_marker" \
        || fail "rootfs libc mismatch"
    mount_chroot_filesystems
else
    prepare_base_rootfs
fi

ensure_lwprintf_cross_compiler
if ! rust_toolchain_ready; then
    chroot_bash "rustup toolchain install '$toolchain' --profile minimal \
        --component rust-src --component llvm-tools-preview"
fi
chroot_bash "rustup default '$toolchain'"
if ! chroot_bash 'command -v rust-nm >/dev/null 2>&1 && command -v rust-objcopy >/dev/null 2>&1'; then
    chroot_bash 'cargo install --locked cargo-binutils'
fi
if ! chroot_bash 'command -v gen_ksym >/dev/null 2>&1'; then
    chroot_bash 'cargo install --locked --version 0.6.0 ksym'
fi

source_parent="$rootfs/opt/tgoskits-sources"
source_dir="$source_parent/$source_sha256"
if [ ! -f "$source_dir/Cargo.toml" ]; then
    source_stage="$source_parent/.stage-$source_sha256-$$"
    install -d -m 0755 "$source_stage"
    tar -xf "$source_archive" -C "$source_stage"
    install -m 0644 "$source_meta" "$source_stage/.tgoskits-source-meta"
    [ -f "$source_stage/Cargo.toml" ] || fail "source archive has no Cargo.toml"
    mv "$source_stage" "$source_dir"
fi
"$app_root/install_source_link.sh" "$rootfs" "$source_sha256"

install -d -m 0755 \
    "$rootfs/work/targets" \
    "$rootfs/output/runs" \
    "$rootfs/etc/starry-selfbuild"
if [ ! -f "$rootfs/etc/starry-selfbuild/run.conf" ]; then
    cat > "$rootfs/etc/starry-selfbuild/run.conf" <<EOF
run_id=manual
profile=off
build_epoch=$(date +%s)
EOF
fi

chroot_bash 'cd /opt/tgoskits && cargo fetch --locked'
chroot_bash 'cd /opt/tgoskits && cargo fetch --locked --target aarch64-unknown-none-softfloat -Z build-std=core,alloc'
chroot_bash 'cd /opt/tgoskits && CARGO_NET_OFFLINE=true cargo metadata --locked --offline --no-deps >/dev/null'

manifest="$rootfs/etc/starry-selfbuild/rootfs.manifest"
{
    cat "$rootfs_marker"
    printf 'toolchain=%s\n' "$toolchain"
    printf 'source_sha256=%s\n' "$source_sha256"
    chroot_bash 'printf "rustc="; rustc --version; printf "cargo="; cargo --version; printf "glibc="; ldd --version | head -n 1'
} > "$manifest"

sync
echo "starry_selfbuild_rootfs=$rootfs"
echo "starry_selfbuild_source_sha256=$source_sha256"
