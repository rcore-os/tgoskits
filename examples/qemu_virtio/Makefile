# Makefile for QEMU VirtIO Example

TARGET := riscv64gc-unknown-none-elf
MODE := release
KERNEL_ELF := target/$(TARGET)/$(MODE)/qemu_virtio_example
KERNEL_BIN := $(KERNEL_ELF).bin
BOOTLOADER := ./bootloader/rustsbi-qemu.bin

# QEMU 参数
QEMU := qemu-system-riscv64
QEMU_ARGS := -machine virt \
             -nographic \
             -bios $(BOOTLOADER) \
             -device loader,file=$(KERNEL_BIN),addr=0x80200000 \
             -drive file=disk.img,if=none,format=raw,id=x0 \
             -device virtio-blk-device,drive=x0 \
             -smp 1 \
             -m 800M

.PHONY: all build run clean disk

all: build

# 构建
build:
	@echo "构建内核..."
	@cargo build --release
	@rust-objcopy --binary-architecture=riscv64 \
		--strip-all \
		-O binary $(KERNEL_ELF) $(KERNEL_BIN)
	@echo "构建完成: $(KERNEL_BIN)"

# 创建磁盘镜像
disk:
	@echo "创建 512MB 磁盘镜像..."
	@dd if=/dev/zero of=disk.img bs=1M count=512
	@echo "磁盘镜像创建完成: disk.img"

# 运行
run: build disk
	@echo "启动 QEMU..."
	@$(QEMU) $(QEMU_ARGS)

# 调试
debug: build disk
	@echo "启动 QEMU (调试模式)..."
	@$(QEMU) $(QEMU_ARGS) -s -S

# 清理
clean:
	@cargo clean
	@rm -f disk.img
	@echo "清理完成"

# 查看内核信息
info:
	@rust-readobj -h $(KERNEL_ELF)

# 反汇编
asm:
	@rust-objdump -S $(KERNEL_ELF) | less
