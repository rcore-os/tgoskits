# Makefile for QEMU VirtIO Example

TARGET := riscv64gc-unknown-none-elf
MODE := release
KERNEL_ELF := target/$(TARGET)/$(MODE)/qemu_virtio_example
KERNEL_BIN := $(KERNEL_ELF).bin
BOOTLOADER := ./bootloader/rustsbi-qemu.bin

BE_TARGET := mips-unknown-linux-gnu
BE_MODE := release
BE_BIN := ../../target/$(BE_TARGET)/$(BE_MODE)/RVlwext4
QEMU_USER := qemu-mips
QEMU_USER_LD := /usr/mips-linux-gnu

CARGO_NIGHTLY := cargo +nightly
BUILD_STD_FLAGS := -Zbuild-std=std,panic_abort

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

.PHONY: build run clean disk be_build be_run
be_build:
	@echo "构建 big-endian usermode..."
	@cd ../.. && \
		CARGO_TARGET_MIPS_UNKNOWN_LINUX_GNU_LINKER=mips-linux-gnu-gcc \
		CARGO_TARGET_MIPS_UNKNOWN_LINUX_GNU_AR=mips-linux-gnu-ar \
		$(CARGO_NIGHTLY) build --release --target $(BE_TARGET) $(BUILD_STD_FLAGS)
	@echo "构建完成: $(BE_BIN)"

be_run: be_build
	@echo "启动 big-endian usermode..."
	@$(QEMU_USER) -L $(QEMU_USER_LD) $(BE_BIN)

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
