if [ "$(id -u)" -ne 0 ]; then
    echo "This script must be run as root"
    exit 1
fi

DEV=$1
FILE=$2

if [ -z "$DEV" ] || [ -z "$FILE" ]; then
    echo "Usage: $0 <device> <file>"
    exit 1
fi

set -e

mount -v $DEV mnt
cp -v ${FILE} mnt/kernel
umount -v mnt
