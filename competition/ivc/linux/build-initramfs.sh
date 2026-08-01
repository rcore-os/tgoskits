#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
output_dir="$workspace/tmp/competition/ivc/linux"
rootfs_image="$output_dir/rootfs.img"
output_archive="$output_dir/initramfs.cpio.gz"
controller="$output_dir/target/aarch64-unknown-linux-musl/release/ivcproto"

"$script_dir/build-rootfs.sh"

mkdir -p "$output_dir"
extract_root=$(mktemp -d "$output_dir/initramfs-root.XXXXXX")
case "$extract_root" in
    "$output_dir"/initramfs-root.*) ;;
    *)
        echo "Refusing to use unexpected extraction directory: $extract_root" >&2
        exit 1
        ;;
esac
cleanup() {
    rm -rf -- "$extract_root"
}
trap cleanup EXIT HUP INT TERM

# Read only the runtime closure from the private ext4 image. The base image
# also contains LTP and development files that are unrelated to the IVC guest
# and would make the initramfs hundreds of megabytes larger.
mkdir -p "$extract_root/bin" "$extract_root/dev" "$extract_root/lib" \
    "$extract_root/proc" "$extract_root/run" "$extract_root/sbin" \
    "$extract_root/sys" "$extract_root/tmp" "$extract_root/usr/local/bin"
debugfs -R "dump /bin/busybox $extract_root/bin/busybox" "$rootfs_image" >/dev/null
debugfs -R \
    "dump /lib/ld-musl-aarch64.so.1 $extract_root/lib/ld-musl-aarch64.so.1" \
    "$rootfs_image" >/dev/null
chmod 0755 "$extract_root/bin/busybox" "$extract_root/lib/ld-musl-aarch64.so.1"
install -m 0755 "$controller" "$extract_root/usr/local/bin/ivcproto"
install -m 0755 "$script_dir/ivc-init.sh" "$extract_root/init"

for applet in cat mount sh sleep sync; do
    ln -s busybox "$extract_root/bin/$applet"
done
for applet in ip poweroff; do
    ln -s ../bin/busybox "$extract_root/sbin/$applet"
done

fakeroot -- sh -s -- "$extract_root" <<'FAKEROOT' | gzip -9 >"$output_archive"
    set -eu
    root=$1
    mknod "$root/dev/console" c 5 1
    chmod 0600 "$root/dev/console"
    cd "$root"
    find . -mindepth 1 -print0 \
        | sort -z \
        | cpio --null --create --format=newc --owner=0:0
FAKEROOT

gzip -t "$output_archive"
sha256sum "$output_archive"
echo "IVC Linux initramfs ready at $output_archive"
