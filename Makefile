# Build Options
export ARCH := riscv64
export LOG := warn
export BACKTRACE := y
export MEMTRACK := n

# QEMU Options
export BLK := y
export NET := y
export MEM := 1G
export ICOUNT := n

# Generated Options
export A := $(PWD)
export NO_AXSTD := y
export AX_LIB := axfeat
export APP_FEATURES := qemu

ifeq ($(MEMTRACK), y)
	APP_FEATURES += starry-api/memtrack
endif

DIR := $(shell basename $(PWD))
IMG = rootfs-$(ARCH).img

defconfig:
	@make -C arceos $@

build run justrun debug disasm: defconfig
	@make -C arceos $@

# Aliases
rv:
	$(MAKE) ARCH=riscv64 run

la:
	$(MAKE) ARCH=loongarch64 run

vf2:
	$(MAKE) ARCH=riscv64 APP_FEATURES=vf2 MYPLAT=axplat-riscv64-visionfive2 BUS=dummy build

2k1000la:
	$(MAKE) ARCH=loongarch64 APP_FEATURES=2k1000la MYPLAT=axplat-loongarch64-2k1000la BUS=dummy build

.PHONY: build run justrun debug disasm clean
