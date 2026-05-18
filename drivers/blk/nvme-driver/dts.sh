SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TARGET_DIR="$SCRIPT_DIR/../../../target"

qemu-system-aarch64 \
	-machine virt,dumpdtb="$TARGET_DIR/qemu.dtb" \
	-display none \
	-cpu cortex-a53 \
	-smp 1 \
	-drive file="$TARGET_DIR/nvme.img",format=raw,if=none,id=nvm \
	-device nvme,serial=deadbeef,drive=nvm
dtc -I dtb -O dts -o "$TARGET_DIR/qemu.dts" "$TARGET_DIR/qemu.dtb"