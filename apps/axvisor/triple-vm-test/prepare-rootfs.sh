#!/usr/bin/env bash
# Run once in the board's native Linux, before the AxVisor session.
set -euo pipefail
if [[ $# -lt 3 || $# -gt 4 || $EUID -ne 0 ]]; then
    echo 'Usage: sudo ./prepare-rootfs.sh DEST KERNEL_IMAGE KERNEL_RELEASE [VIRTIO_MODULE_DIR]' >&2
    exit 2
fi
stage=$(realpath -m -- "$1")
kernel=$(realpath -e -- "$2")
release=$3
module_dir=${4:-}
[[ -d /lib/modules/$release && -s /var/lib/dpkg/status ]] || {
    echo 'A complete rootfs and the matching kernel modules are required.' >&2
    exit 1
}
if pgrep -x apt-get >/dev/null || pgrep -x dpkg >/dev/null; then
    echo 'Wait for package management to finish before preparing the image.' >&2
    exit 1
fi
if [[ -n $module_dir ]]; then
    module_dir=$(realpath -e -- "$module_dir")
    for module in virtio_mmio virtio_blk; do
        vermagic=$(modinfo -F vermagic "$module_dir/$module.ko")
        [[ $vermagic == "$release "* ]] || {
            echo "Module $module does not match kernel release $release" >&2
            exit 1
        }
    done
fi
umask 077
mkdir "$stage"
install -m 600 "$kernel" "$stage/linux-Image"
root=$stage/root
image=$stage/linux-rootfs.ext4
mkdir "$root"
truncate -s "${ROOTFS_SIZE:-8G}" "$image"
mkfs.ext4 -q -F -L AXTRIPLE_ROOT "$image"
mount -o loop "$image" "$root"
cleanup() {
    for mountpoint in "$root/dev" "$root/proc" "$root/sys" "$root"; do
        if mountpoint -q "$mountpoint"; then umount "$mountpoint"; fi
    done
}
trap cleanup EXIT
rsync -aHAXx --numeric-ids \
    --exclude=/dev/*** --exclude=/proc/*** --exclude=/sys/*** \
    --exclude=/tmp/*** --exclude=/run/*** --exclude=/mnt/*** \
    --exclude=/media/*** --exclude=/guest/*** --exclude=/lost+found \
    --exclude=/var/log/*** --exclude=/var/log.hdd/*** \
    --exclude="$stage/***" / "$root/"
mkdir -p "$root"/{dev,proc,sys,tmp,run,mnt,media,var/log,var/log.hdd}
chmod 1777 "$root/tmp"
printf '/dev/vda / ext4 defaults 0 1\n' > "$root/etc/fstab"
mkdir -p "$root/etc/initramfs-tools/conf.d"
# /dev/vda exists only in the guest, so the preparation host cannot probe it.
printf 'FSTYPE=ext4\n' > "$root/etc/initramfs-tools/conf.d/triple-rootfs"
if [[ -n $module_dir ]]; then
    mkdir -p "$root/lib/modules/$release/extra/triple"
    cp "$module_dir/virtio_mmio.ko" "$module_dir/virtio_blk.ko" \
        "$root/lib/modules/$release/extra/triple/"
fi
printf '\nvirtio_mmio\nvirtio_blk\n' >> "$root/etc/initramfs-tools/modules"
depmod -b "$root" "$release"
mount --bind /dev "$root/dev"
mount -t proc proc "$root/proc"
mount --bind /sys "$root/sys"
chroot "$root" mkinitramfs -o /tmp/triple-initrd.img "$release"
cp "$root/tmp/triple-initrd.img" "$stage/linux-initrd.img"
rm "$root/tmp/triple-initrd.img"
chroot "$root" dpkg --audit
sync
cleanup
trap - EXIT
rmdir "$root"
e2fsck -fn "$image"
cmp "$kernel" "$stage/linux-Image"
sha256sum "$stage/linux-Image" "$stage/linux-initrd.img"
printf 'Prepared full Linux rootfs: %s\n' "$image"
