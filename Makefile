# Build Options
export ARCH := riscv64
export LOG := warn
export BACKTRACE := y
export MEMTRACK := n

# QEMU Options
export BLK := y
export NET := y
export MEM := 1G
export TEST := pre

# Generated Options
export A := $(PWD)
export NO_AXSTD := y
export AX_LIB := axfeat
export APP_FEATURES := qemu

ifeq ($(MEMTRACK), y)
	APP_FEATURES += starry-api/memtrack
endif

export ICOUNT := n

DIR := $(shell basename $(PWD))

all:
	@if [ -f cargo_config.toml ]; then \
		mkdir -p .cargo; \
		cp cargo_config.toml .cargo/config.toml; \
	fi
	@if [ -d bin ]; then \
		cp bin/* ~/.cargo/bin; \
	fi
	export RUSTUP_TOOLCHAIN=nightly-2025-05-20 && \
		export CARGO_PROFILE_RELEASE_LTO=true && \
		export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 && \
		$(MAKE) ARCH=riscv64 BUS=mmio LOG=off BACKTRACE=n build && \
		$(MAKE) ARCH=loongarch64 LOG=off BACKTRACE=n build
	cp $(DIR)_riscv64-qemu-virt.bin kernel-rv
	cp $(DIR)_loongarch64-qemu-virt.elf kernel-la

IMG :=
ifeq ($(ARCH), riscv64)
	IMG := sdcard-rv-$(TEST).img
else ifeq ($(ARCH), loongarch64)
	IMG := sdcard-la-$(TEST).img
endif

run: defconfig
	@if [ -f "$(IMG)" ]; then \
		cp $(IMG) arceos/disk.img; \
	fi
	@make -C arceos run

# Aliases
rv:
	$(MAKE) ARCH=riscv64 run

la:
	$(MAKE) ARCH=loongarch64 run

alpine:
	$(MAKE) TEST=alpine rv

vf2:
	$(MAKE) ARCH=riscv64 APP_FEATURES=vf2 MYPLAT=axplat-riscv64-visionfive2 BUS=dummy build

build justrun debug disasm: defconfig
	@make -C arceos $@

defconfig:
	@make -C arceos $@

.PHONY: all build run justrun debug disasm clean
