export ARCH := riscv64
export LOG := warn

export A = $(PWD)
export NO_AXSTD = y
export AX_LIB = axfeat
export BLK = y
export NET = y

DIR := $(shell basename $(PWD))

all:
	mkdir .cargo
	cp cargo_config.toml .cargo/config.toml
	cp bin/* ~/.cargo/bin
	tar -xf vendor.tar.gz
	RUSTUP_TOOLCHAIN=nightly-2025-01-18 $(MAKE) ARCH=riscv64 BUS=mmio LOG=off build
	cp $(DIR)_riscv64-qemu-virt.bin kernel-rv
	RUSTUP_TOOLCHAIN=nightly-2025-01-18 $(MAKE) ARCH=loongarch64 LOG=off build
	cp $(DIR)_loongarch64-qemu-virt.elf kernel-la

IMG_URL = https://github.com/oscomp/testsuits-for-oskernel/releases/download/pre-20250615/

ifeq ($(ARCH), riscv64)
	IMG := sdcard-rv.img
else ifeq ($(ARCH), loongarch64)
	IMG := sdcard-la.img
else
	$(error Unsupported architecture: $(ARCH))
endif

oscomp_run:
	@if [ ! -f $(PWD)/$(IMG) ]; then \
		wget $(IMG_URL)/$(IMG).xz; \
		xz -d $(IMG).xz; \
	fi
	cp $(IMG) arceos/disk.img
	$(MAKE) run

rv:
	$(MAKE) ARCH=riscv64 oscomp_run

la:
	$(MAKE) ARCH=loongarch64 oscomp_run

build run justrun debug disasm: defconfig
	@make -C arceos $@

defconfig:
	@make -C arceos $@

.PHONY: all build run justrun debug disasm clean
