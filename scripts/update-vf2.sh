if [ "$(id -u)" -ne 0 ]; then
    echo "This script must be run as root"
    exit 1
fi

DEV=$1

if [ -z "$DEV" ]; then
    echo "Usage: $0 <device>"
    exit 1
fi

mount $DEV mnt
cp starry-mix_visionfive2.bin mnt/kernel
umount mnt
