#!/usr/bin/env bash
#
# Build a Debian rootfs (ext4 image) for Starry OS using Docker + debootstrap.
#
# Usage:
#   ./scripts/build-debian-rootfs.sh [OPTIONS]
#
# Options:
#   -a, --arch ARCH       Target architecture (default: aarch64)
#   -s, --size SIZE       Image size (default: 2G, systemd: 4G)
#   -o, --output PATH     Output image path (default: auto-detected from arch)
#   -d, --debian VER      Debian suite (default: trixie)
#   -p, --password PASS   Root password (default: root)
#   -i, --init INIT       Init system: busybox (default) or systemd
#   -h, --help            Show this help
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Defaults
ARCH="aarch64"
IMAGE_SIZE="2G"
DEBIAN_SUITE="trixie"
ROOT_PASSWORD="root"
INIT_SYSTEM="busybox"
OUTPUT_PATH=""

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# //; s/^#//'
    exit 0
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -a|--arch)       ARCH="$2"; shift 2 ;;
            -s|--size)       IMAGE_SIZE="$2"; shift 2 ;;
            -o|--output)     OUTPUT_PATH="$2"; shift 2 ;;
            -d|--debian)     DEBIAN_SUITE="$2"; shift 2 ;;
            -p|--password)   ROOT_PASSWORD="$2"; shift 2 ;;
            -i|--init)       INIT_SYSTEM="$2"; shift 2 ;;
            -h|--help)       usage ;;
            *) echo "Unknown option: $1"; usage ;;
        esac
    done
}

resolve_target_and_output() {
    case "$ARCH" in
        aarch64)
            TARGET="aarch64-unknown-none-softfloat"
            DOCKER_ARCH="arm64v8"
            DEB_ARCH="arm64"
            ;;
        riscv64)
            TARGET="riscv64gc-unknown-none-elf"
            DOCKER_ARCH="riscv64"
            DEB_ARCH="riscv64"
            ;;
        x86_64)
            TARGET="x86_64-unknown-none"
            DOCKER_ARCH="amd64"
            DEB_ARCH="amd64"
            ;;
        *)
            echo "Error: unsupported architecture '$ARCH'"
            exit 1
            ;;
    esac

    if [[ -z "$OUTPUT_PATH" ]]; then
        OUTPUT_PATH="$WORKSPACE_ROOT/target/$TARGET/rootfs-$ARCH.img"
    fi
}

check_docker() {
    if ! command -v docker &>/dev/null; then
        echo "Error: docker not found. Please install Docker first."
        exit 1
    fi
    if ! docker info &>/dev/null; then
        echo "Error: Docker daemon is not running."
        exit 1
    fi
}

check_cross_arch() {
    local host_arch
    host_arch="$(uname -m)"
    # Map host arch to Debian arch naming
    local host_deb_arch
    case "$host_arch" in
        x86_64)  host_deb_arch="amd64" ;;
        aarch64) host_deb_arch="arm64" ;;
        riscv64) host_deb_arch="riscv64" ;;
        *)       host_deb_arch="$host_arch" ;;
    esac

    # Only need binfmt when target != host
    if [[ "$DEB_ARCH" == "$host_deb_arch" ]]; then
        return 0
    fi

    # Verify cross-arch emulation actually works by running a tiny container
    echo "    Checking cross-arch emulation ($host_arch -> $DEB_ARCH)..."
    if docker run --rm --platform "linux/$DEB_ARCH" \
        "${DOCKER_ARCH}/debian:${DEBIAN_SUITE}" echo ok &>/dev/null; then
        return 0
    fi

    echo "Error: Cross-architecture build ($DEB_ARCH) on $host_arch requires QEMU binfmt emulation."
    echo ""
    echo "  To fix, run:"
    echo "    docker run --rm --privileged multiarch/qemu-user-static --reset -p yes"
    echo ""
    echo "  Or build for the native architecture instead:"
    case "$host_arch" in
        x86_64)  echo "    $0 --init $INIT_SYSTEM --arch x86_64" ;;
        aarch64) echo "    $0 --init $INIT_SYSTEM --arch aarch64" ;;
        riscv64) echo "    $0 --init $INIT_SYSTEM --arch riscv64" ;;
    esac
    exit 1
}

build_rootfs() {
    local vol_name="starry-rootfs-build-$$"

    echo "==> Building Debian $DEBIAN_SUITE rootfs for $ARCH ($DEB_ARCH)..."
    echo "    Docker image: ${DOCKER_ARCH}/debian:${DEBIAN_SUITE}"
    echo "    Output: $OUTPUT_PATH"
    echo ""

    # Create a named Docker volume to avoid bind-mount nodev/noexec issues
    docker volume create "$vol_name" >/dev/null

    cleanup_volume() {
        docker volume rm "${vol_name:-}" >/dev/null 2>&1 || true
    }
    trap cleanup_volume EXIT

    # Step 1: Run debootstrap + configure rootfs inside Docker using a named volume
    echo "==> [1/2] Running debootstrap and configuring rootfs..."
    docker run --rm \
        --platform "linux/$DEB_ARCH" \
        -v "${vol_name}:/rootfs" \
        "${DOCKER_ARCH}/debian:${DEBIAN_SUITE}" \
        bash -c "
            set -e

            # --- replace container apt source with TUNA mirror ---
            cat > /etc/apt/sources.list <<'CONTAINER_SRC'
deb http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie main contrib non-free non-free-firmware
deb-src http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie main contrib non-free non-free-firmware

deb http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie-updates main contrib non-free non-free-firmware
deb-src http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie-updates main contrib non-free non-free-firmware

deb http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie-backports main contrib non-free non-free-firmware
deb-src http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie-backports main contrib non-free non-free-firmware

deb http://mirrors4.tuna.tsinghua.edu.cn/debian-security trixie-security main contrib non-free non-free-firmware
deb-src http://mirrors4.tuna.tsinghua.edu.cn/debian-security trixie-security main contrib non-free non-free-firmware
CONTAINER_SRC

            apt-get update
            apt-get install -y debootstrap e2fsprogs busybox-static

            # --- debootstrap ---
            debootstrap --arch=$DEB_ARCH --variant=minbase --no-merged-usr \
                $DEBIAN_SUITE /rootfs http://mirrors.tuna.tsinghua.edu.cn/debian

            ROOTFS=/rootfs

            # --- sources.list (use TUNA mirror) ---
            cat > \$ROOTFS/etc/apt/sources.list <<'SOURCES'
deb http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie main contrib non-free non-free-firmware
deb-src http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie main contrib non-free non-free-firmware

deb http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie-updates main contrib non-free non-free-firmware
deb-src http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie-updates main contrib non-free non-free-firmware

deb http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie-backports main contrib non-free non-free-firmware
deb-src http://mirrors4.tuna.tsinghua.edu.cn/debian/ trixie-backports main contrib non-free non-free-firmware

deb http://mirrors4.tuna.tsinghua.edu.cn/debian-security trixie-security main contrib non-free non-free-firmware
deb-src http://mirrors4.tuna.tsinghua.edu.cn/debian-security trixie-security main contrib non-free non-free-firmware
SOURCES

            # --- hostname ---
            echo 'starry' > \$ROOTFS/etc/hostname
            echo '127.0.0.1 localhost starry' > \$ROOTFS/etc/hosts

            # --- fstab ---
            cat > \$ROOTFS/etc/fstab <<'FSTAB'
/dev/vda  /  ext4  defaults,noatime  0  1
FSTAB

            # --- set root password ---
            echo 'root:$ROOT_PASSWORD' | chroot \$ROOTFS chpasswd

            # --- install busybox-static and ensure full libc6 (NSS modules) ---
            chroot \$ROOTFS apt-get update
            chroot \$ROOTFS apt-get install -y --reinstall libc6
            chroot \$ROOTFS apt-get install -y busybox-static bash

            # --- busybox init setup (only when --init busybox) ---
            if [ "$INIT_SYSTEM" = "busybox" ]; then
                # ensure /sbin/init is busybox
                if [ ! -L \$ROOTFS/sbin/init ] && [ ! -e \$ROOTFS/sbin/init ]; then
                    ln -sf /bin/busybox \$ROOTFS/sbin/init
                fi

                # inittab for busybox init
                cat > \$ROOTFS/etc/inittab <<'INITTAB'
# /etc/inittab - busybox init for Starry OS
::sysinit:/etc/init.d/rcS
::respawn:-/bin/sh
::shutdown:/bin/umount -a -r
INITTAB

                # rcS startup
                mkdir -p \$ROOTFS/etc/init.d
                cat > \$ROOTFS/etc/init.d/rcS <<'RCS'
#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mkdir -p /dev/pts
mount -t devpts devpts /dev/pts 2>/dev/null
hostname starry
export HOME=/root
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
RCS
                chmod +x \$ROOTFS/etc/init.d/rcS
            fi

            # --- systemd setup (if requested) ---
            if [ "$INIT_SYSTEM" = "systemd" ]; then
                echo 'Installing systemd...'
                chroot \$ROOTFS apt-get install -y systemd

                # Ensure /sbin/init points to systemd (overwrite any existing symlink)
                ln -sf /lib/systemd/systemd \$ROOTFS/sbin/init

                # Remove busybox inittab if it exists (systemd doesn't use it)
                rm -f \$ROOTFS/etc/inittab

                # machine-id (empty = will be generated on first boot)
                : > \$ROOTFS/etc/machine-id
                chmod 444 \$ROOTFS/etc/machine-id

                # Ensure mount points exist
                mkdir -p \$ROOTFS/proc \$ROOTFS/sys \$ROOTFS/dev \$ROOTFS/dev/pts
                mkdir -p \$ROOTFS/run \$ROOTFS/tmp \$ROOTFS/var/run \$ROOTFS/var/log

                # Create minimal /sys structure to prevent systemd chase() crash
                mkdir -p \$ROOTFS/sys/class \$ROOTFS/sys/block \$ROOTFS/sys/dev
                mkdir -p \$ROOTFS/sys/devices/system/cpu
                mkdir -p \$ROOTFS/sys/fs/cgroup
                mkdir -p \$ROOTFS/sys/subsystem
                echo '1' > \$ROOTFS/sys/devices/system/cpu/online

                # Disable units that won't work in Starry OS
                chroot \$ROOTFS systemctl mask systemd-remount-fs.service
                chroot \$ROOTFS systemctl mask systemd-tmpfiles-setup.service
                chroot \$ROOTFS systemctl mask systemd-tmpfiles-setup-dev.service
                chroot \$ROOTFS systemctl mask systemd-udevd.service
                chroot \$ROOTFS systemctl mask systemd-udevd-control.socket
                chroot \$ROOTFS systemctl mask systemd-udevd-kernel.socket
                chroot \$ROOTFS systemctl mask systemd-journald.service
                chroot \$ROOTFS systemctl mask systemd-journald-dev-log.socket
                chroot \$ROOTFS systemctl mask systemd-journald-audit.socket
                chroot \$ROOTFS systemctl mask systemd-journald.socket
                chroot \$ROOTFS systemctl mask systemd-networkd.service
                chroot \$ROOTFS systemctl mask systemd-networkd.socket
                chroot \$ROOTFS systemctl mask systemd-resolved.service
                chroot \$ROOTFS systemctl mask systemd-logind.service
                chroot \$ROOTFS systemctl mask systemd-machined.service
                chroot \$ROOTFS systemctl mask systemd-importd.service
                chroot \$ROOTFS systemctl mask systemd-hostnamed.service
                chroot \$ROOTFS systemctl mask systemd-localed.service
                chroot \$ROOTFS systemctl mask systemd-timedated.service
                chroot \$ROOTFS systemctl mask systemd-portabled.service
                chroot \$ROOTFS systemctl mask dbus.socket
                chroot \$ROOTFS systemctl mask dbus.service
                chroot \$ROOTFS systemctl mask systemd-random-seed.service
                chroot \$ROOTFS systemctl mask kmod-static-nodes.service
                chroot \$ROOTFS systemctl mask systemd-modules-load.service
                chroot \$ROOTFS systemctl mask sys-kernel-config.mount
                chroot \$ROOTFS systemctl mask sys-kernel-debug.mount
                chroot \$ROOTFS systemctl mask sys-fs-fuse-connections.mount
                chroot \$ROOTFS systemctl mask systemd-update-utmp.service
                chroot \$ROOTFS systemctl mask systemd-update-done.service
                chroot \$ROOTFS systemctl mask systemd-sysctl.service
                chroot \$ROOTFS systemctl mask systemd-ask-password-wall.path
                chroot \$ROOTFS systemctl mask paths.target

                # Set default target to multi-user
                chroot \$ROOTFS systemctl set-default multi-user.target

                # Console getty: use bash directly (agetty enters serial baud rate
                # detection on ttyAMA0 and times out; -L flag doesn't prevent it).
                cat > \$ROOTFS/etc/systemd/system/console-getty.service <<'GETTY'
[Unit]
Description=Console Shell on Starry OS
After=systemd-user-sessions.service

[Service]
Type=simple
ExecStart=-/bin/bash --login
Restart=always
RestartSec=0
KillMode=process

StandardInput=tty
StandardOutput=tty
StandardError=tty
TTYPath=/dev/console

[Install]
WantedBy=getty.target
GETTY
                chroot \$ROOTFS systemctl enable console-getty.service

                # Mask serial-getty@ttyAMA0 to prevent systemd auto-detection from
                # spawning agetty with serial baud rate detection
                ln -sf /dev/null \$ROOTFS/etc/systemd/system/serial-getty@ttyAMA0.service
            fi

            # --- APT config for Starry OS ---
            mkdir -p \$ROOTFS/etc/apt/apt.conf.d
            echo 'APT::Sandbox::User "root";' > \$ROOTFS/etc/apt/apt.conf.d/99no-sandbox
            echo 'APT::Cache-Start "67108864";' > \$ROOTFS/etc/apt/apt.conf.d/99cache-start

            # --- welcome script ---
            mkdir -p \$ROOTFS/root
            cat > \$ROOTFS/root/init.sh <<'INIT_SH'
#!/bin/sh
export HOME=/root
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
echo ''
echo 'Welcome to Starry OS (Debian GNU/Linux)'
echo ''
echo 'Use apt to install packages.'
echo ''
cd ~
sh --login
INIT_SH
            chmod +x \$ROOTFS/root/init.sh

            # --- profile ---
            cat > \$ROOTFS/root/.profile <<'PROFILE'
export PS1='starry:~# '
PROFILE

            # --- network ---
            mkdir -p \$ROOTFS/etc/network
            cat > \$ROOTFS/etc/network/interfaces <<'NETIF'
auto eth0
iface eth0 inet dhcp
NETIF

            # --- clean up ---
            chroot \$ROOTFS apt-get clean
            rm -rf \$ROOTFS/var/lib/apt/lists/*
            rm -rf \$ROOTFS/var/cache/apt/archives/*.deb

            # --- resolv.conf (MUST be after cleanup — Docker overwrites it) ---
            cat > \$ROOTFS/etc/resolv.conf <<'RESOLV'
nameserver 10.0.2.3
nameserver 8.8.8.8
RESOLV
        "

    # Step 2: Create ext4 image inside Docker (no sudo needed on host)
    echo "==> [2/2] Creating ${IMAGE_SIZE} ext4 image..."
    mkdir -p "$(dirname "$OUTPUT_PATH")"

    local output_dir
    output_dir="$(dirname "$OUTPUT_PATH")"
    local output_file
    output_file="$(basename "$OUTPUT_PATH")"

    docker run --rm --privileged \
        --platform "linux/$DEB_ARCH" \
        -v "${vol_name}:/rootfs:ro" \
        -v "${output_dir}":/output \
        "${DOCKER_ARCH}/debian:${DEBIAN_SUITE}" \
        bash -c "
            set -e
            echo 'deb http://mirrors.tuna.tsinghua.edu.cn/debian/ $DEBIAN_SUITE main' > /etc/apt/sources.list
            apt-get update && apt-get install -y e2fsprogs >/dev/null 2>&1
            cd /output
            dd if=/dev/zero of=$output_file bs=1 count=0 seek=$IMAGE_SIZE 2>/dev/null
            mkfs.ext4 -F -L starry-rootfs -O ^orphan_file,^metadata_csum_seed $output_file
            mkdir -p /mnt/rootfs
            mount -o loop $output_file /mnt/rootfs
            cp -a /rootfs/. /mnt/rootfs/
            sync
            umount /mnt/rootfs
            rmdir /mnt/rootfs
        "

    cleanup_volume
    trap - EXIT

    local img_size
    img_size=$(du -h "$OUTPUT_PATH" | cut -f1)
    echo ""
    echo "==> Done!"
    echo "    Image: $OUTPUT_PATH ($img_size)"
    echo ""
    echo "    To boot with Starry:"
    echo "      cargo starry qemu --arch $ARCH"
}

main() {
    parse_args "$@"
    resolve_target_and_output
    check_docker
    check_cross_arch

    # systemd needs more space
    if [[ "$INIT_SYSTEM" == "systemd" && "$IMAGE_SIZE" == "2G" ]]; then
        IMAGE_SIZE="4G"
    fi

    build_rootfs
}

main "$@"
