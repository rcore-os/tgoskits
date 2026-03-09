SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
dd if=/dev/zero bs=1M count=128 of="$SCRIPT_DIR/../../../target/nvme.img"