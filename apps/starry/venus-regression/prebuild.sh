#!/usr/bin/env bash
set -euo pipefail
if ! linker=$(command -v ld.lld); then
    echo 'venus-regression: ld.lld is required; install LLD (Ubuntu/Debian: apt-get install lld; Fedora: dnf install lld) and add it to PATH.' >&2
    exit 1
fi
# This application exercises CPU mappings of host Vulkan memory. On Intel xe
# the required KVM/PAT contract is only available in QEMU 11 and newer.
python3 - <<'CHECK_QEMU'
import re, subprocess
version = subprocess.check_output(["qemu-system-x86_64", "--version"], text=True)
match = re.search(r"version (\d+)\.", version)
if not match or int(match.group(1)) < 11:
    raise SystemExit("venus-regression requires QEMU >= 11 with KVM honor-guest-pat; ordinary 2D suites are unaffected")
CHECK_QEMU
: "${STARRY_STAGING_ROOT:?}" "${STARRY_OVERLAY_DIR:?}" "${STARRY_ROOTFS:?}" "${STARRY_APP_DIR:?}"
# rdump requires complete ownership semantics before it starts, not a retry
# after a partially extracted image. Keep fakeroot active through preparation.
if [[ $EUID != 0 && -z ${FAKEROOTKEY:-} ]]; then
    exec fakeroot bash "$0"
fi
stage="$STARRY_STAGING_ROOT"
overlay="$STARRY_OVERLAY_DIR"
debugfs -R "rdump / $stage" "$STARRY_ROOTFS"
test -x "$stage/sbin/apk" && test -d "$stage/usr/lib"
cp /etc/resolv.conf "$stage/etc/resolv.conf"
# Alpine 3.23 does not package the Venus ICD. Use the fixed 3.24 repositories
# only in this disposable application image, preserving the selected mirror.
sed 's@/v3\.23/@/v3.24/@g' "$stage/etc/apk/repositories" > "$stage/etc/apk/venus-repositories"
mv "$stage/etc/apk/venus-repositories" "$stage/etc/apk/repositories"
runner="${QEMU_X86_64_STATIC:-qemu-x86_64-static}"
cache="${STARRY_WORKSPACE:?}/target/venus-apk-cache"
mkdir -p "$cache"
for attempt in 1 2 3; do
    if QEMU_LD_PREFIX="$stage" LD_LIBRARY_PATH="$stage/lib:$stage/usr/lib" \
        "$runner" -U LD_PRELOAD -L "$stage" "$stage/sbin/apk" \
        --root "$stage" --repositories-file "$stage/etc/apk/repositories" \
        --keys-dir "$stage/etc/apk/keys" --cache-dir "$cache" --update-cache --timeout 60 \
        --force-no-chroot --scripts=no add vulkan-loader-dev mesa-vulkan-virtio libdrm-dev; then
        break
    fi
    [[ $attempt != 3 ]] || exit 1
    sleep "$((attempt * 5))"
done
mkdir -p "$overlay/usr/bin" "$overlay/usr/share"
linker_dir="$(dirname "$linker")"
x86_64-linux-musl-gcc -B"$linker_dir/" -fuse-ld=lld --sysroot="$stage" -std=c11 -Wall -Wextra -Werror \
    "$STARRY_APP_DIR/main.c" "$STARRY_APP_DIR/execbuf_signal.c" \
    -L"$stage/usr/lib" -Wl,-rpath-link,"$stage/usr/lib" \
    -lvulkan -o "$overlay/usr/bin/venus-regression"
cp -a --reflink=auto "$stage/lib" "$overlay/"
cp -a --reflink=auto "$stage/usr/lib" "$overlay/usr/"
cp -a "$stage/usr/share/vulkan" "$overlay/usr/share/"
test -n "$(find "$overlay/usr/share/vulkan/icd.d" -name 'virtio_icd*.json' -print -quit)"
