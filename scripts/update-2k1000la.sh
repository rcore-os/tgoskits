if [ "$(id -u)" -ne 0 ]; then
    echo "This script must be run as root"
    exit 1
fi

DEV=$1

if [ -z "$DEV" ]; then
    echo "Usage: $0 <device>"
    exit 1
fi

set -e

mount $DEV mnt
cp starry-mix_loongarch64-2k1000la.bin mnt/kernel
umount mnt
