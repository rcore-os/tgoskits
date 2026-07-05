# Available arguments:
# * General options:
#     - `ARCH`: Target architecture: x86_64, riscv64, aarch64, loongarch64
#     - `SMP`: Number of CPUs.
#     - `MODE`: Build mode: release, debug
#     - `LOG:` Logging level: warn, error, info, debug, trace
#     - `V`: Verbose level: (empty), 1, 2
#     - `TARGET_DIR`: Artifact output directory (cargo target directory)
#     - `UIMAGE`: To generate U-Boot image
#     - `LD_SCRIPT`: Use a custom linker script file.
#     - `LINKER_SCRIPT_DIRS`: Extra linker script search directories for C apps.
# * App options:
#     - `A` or `APP`: Path to the application
#     - `FEATURES`: Features os ArceOS modules to be enabled.
#     - `APP_FEATURES`: Features of (rust) apps to be enabled.
# * QEMU options:
#     - `BLK`: Enable storage devices (virtio-blk)
#     - `NET`: Enable network devices (virtio-net)
#     - `GRAPHIC`: Enable display devices and graphic output (virtio-gpu)
#     - `BUS`: Device bus type: mmio, pci
#     - `MEM`: Memory size (default is 128M)
#     - `DISK_IMG`: Path to the virtual disk image
#     - `ACCEL`: Enable hardware acceleration (KVM on linux)
#     - `QEMU_LOG`: Enable QEMU logging (log file is "qemu.log")
#     - `NET_DUMP`: Enable network packet dump (log file is "netdump.pcap")
#     - `NET_DEV`: QEMU netdev backend types: user, tap, bridge
#     - `VFIO_PCI`: PCI device address in the format "bus:dev.func" to passthrough
#     - `VHOST`: Enable vhost-net for tap backend (only for `NET_DEV=tap`)
# * Network options:
#     - `IP`: ArceOS IPv4 address (default is 10.0.2.15 for QEMU user netdev)
#     - `GW`: Gateway IPv4 address (default is 10.0.2.2 for QEMU user netdev)

# General options
ARCH ?= x86_64
TGOSKITS_ROOT ?= $(abspath $(CURDIR)/../..)
SMP ?= 1
MODE ?= release
LOG ?= warn
V ?=
DWARF ?=
LTO ?=
TARGET_DIR ?= $(PWD)/target
UIMAGE ?= n

# App options
A ?= examples/helloworld
APP ?= $(A)
FEATURES ?=
APP_FEATURES ?=
NO_AXSTD ?= n

# QEMU options
BLK ?= n
NET ?= n
GRAPHIC ?= n
INPUT ?= n
VSOCK ?= n
BUS ?= pci
MEM ?= 128M
ACCEL ?=
ICOUNT ?= n
QEMU_ARGS ?=

DISK_IMG ?= disk.img
QEMU_LOG ?= n
NET_DUMP ?= n
NET_DEV ?= user
VFIO_PCI ?=
VHOST ?= n

# Network options
IP ?= 10.0.2.15
GW ?= 10.0.2.2

# App type
ifeq ($(wildcard $(APP)),)
  $(error Application path "$(APP)" is not valid)
endif

ifneq ($(wildcard $(APP)/Cargo.toml),)
  APP_TYPE := rust
  AX_LIB ?= ax-std
  CARGO_MANIFEST_DIR := $(abspath $(APP))
else
  APP_TYPE := c
  AX_LIB ?= ax-libc
  CARGO_MANIFEST_DIR := $(TGOSKITS_ROOT)
endif

.DEFAULT_GOAL := all

ifneq ($(filter $(or $(MAKECMDGOALS), $(.DEFAULT_GOAL)), all build disasm run justrun debug defconfig oldconfig),)
# Install dependencies
include scripts/make/deps.mk
# Feature parsing
include scripts/make/features.mk
endif

# Target
ifeq ($(ARCH), x86_64)
  TARGET := x86_64-unknown-none
else ifeq ($(ARCH), aarch64)
  TARGET := aarch64-unknown-none-softfloat
else ifeq ($(ARCH), riscv64)
  TARGET := riscv64gc-unknown-none-elf
else ifeq ($(ARCH), loongarch64)
  TARGET := loongarch64-unknown-none-softfloat
else
  $(error "ARCH" must be one of "x86_64", "riscv64", "aarch64" or "loongarch64")
endif

export AX_ARCH=$(ARCH)
export AX_MODE=$(MODE)
export AX_LOG=$(LOG)
export AX_TARGET=$(TARGET)
export AX_IP=$(IP)
export AX_GW=$(GW)

unexport AX_CONFIG_PATH

# Binutils
CROSS_COMPILE ?= $(ARCH)-linux-musl-
CC := $(CROSS_COMPILE)gcc
AR := $(CROSS_COMPILE)ar
RANLIB := $(CROSS_COMPILE)ranlib
LD := rust-lld -flavor gnu

OBJDUMP ?= rust-objdump -d --print-imm-hex --x86-asm-syntax=intel
OBJCOPY ?= rust-objcopy --binary-architecture=$(ARCH)
GDB ?= gdb

# Paths
OUT_DIR ?= $(APP)
LD_SCRIPT ?= $(shell find "$(TARGET_DIR)/$(TARGET)/$(MODE)/build" -path '*/ax-runtime-*/out/linker.x' -printf '%T@ %p\n' 2>/dev/null | sort -rn | sed -n '1s/^[^ ]* //p')
AXPLAT_SCRIPT ?= $(shell find "$(TARGET_DIR)/$(TARGET)/$(MODE)/build" -path '*/axplat-dyn-*/out/axplat.x' -printf '%T@ %p\n' 2>/dev/null | sort -rn | sed -n '1s/^[^ ]* //p')
LINKER_SCRIPT_DIRS ?= $(sort $(dir $(LD_SCRIPT) $(AXPLAT_SCRIPT)))

APP_NAME := $(shell basename $(APP))
OUT_ELF := $(OUT_DIR)/$(APP_NAME)_$(ARCH).elf
OUT_BIN := $(patsubst %.elf,%.bin,$(OUT_ELF))
OUT_UIMG := $(patsubst %.elf,%.uimg,$(OUT_ELF))
ifeq ($(UIMAGE), y)
  FINAL_IMG := $(OUT_UIMG)
else
  FINAL_IMG := $(OUT_BIN)
endif

all: build

include scripts/make/utils.mk
include scripts/make/build.mk
include scripts/make/qemu.mk

defconfig:
	$(error axconfig generation has been removed; use cargo xtask build configs)

oldconfig:
	$(error axconfig generation has been removed; use cargo xtask build configs)

build: $(OUT_DIR) $(FINAL_IMG)

disasm:
	$(OBJDUMP) $(OUT_ELF) | less

run: build justrun

justrun:
	$(call run_qemu)

debug: build
	$(call run_qemu_debug) &
	$(GDB) $(OUT_ELF) \
	  -ex 'target remote localhost:1234' \
	  -ex 'b __axplat_main' \
	  -ex 'continue' \
	  -ex 'disp /16i $$pc'

clippy:
ifeq ($(origin ARCH), command line)
	$(call cargo_clippy,--target $(TARGET))
else
	$(call cargo_clippy)
endif

doc:
	$(call cargo_doc)

doc_check_missing:
	$(call cargo_doc)

fmt:
	cargo fmt --all

fmt_c:
	@clang-format --style=file -i $(shell find ulib/axlibc -iname '*.c' -o -iname '*.h')

unittest:
	$(call unit_test)

unittest_no_fail_fast:
	$(call unit_test,--no-fail-fast)

disk_img:
ifneq ($(wildcard $(DISK_IMG)),)
	@printf "$(YELLOW_C)warning$(END_C): disk image \"$(DISK_IMG)\" already exists!\n"
else
	$(call make_disk_image,fat32,$(DISK_IMG))
endif

clean: clean_c
	rm -rf $(APP)/*.bin $(APP)/*.elf
	cargo clean --target-dir $(TARGET_DIR)

clean_c::
	rm -rf ulib/axlibc/build_*
	rm -rf $(app-objs)
	rm -rf $(app_obj_dir)

.PHONY: all defconfig oldconfig \
	build disasm run justrun debug \
	clippy doc doc_check_missing fmt fmt_c unittest unittest_no_fail_fast \
	disk_img clean clean_c
