# Compatibility Makefile for the StarryOS subtree.
#
# The supported entry point is `cargo xtask starry ...` from the workspace
# root. This subtree has its own .cargo/config.toml alias, so every recipe that
# invokes xtask first changes to the root workspace. BUILD_CONFIG and
# QEMU_CONFIG are therefore interpreted relative to that root.

STARRY_DIR := $(patsubst %/,%,$(dir $(abspath $(lastword $(MAKEFILE_LIST)))))
ROOT_DIR := $(abspath $(STARRY_DIR)/../..)
XTASK := cd "$(ROOT_DIR)" && cargo xtask starry

ARCH ?= riscv64
SMP ?=
BUILD_CONFIG ?=
QEMU_CONFIG ?=
BOARD ?=

STARRY_BUILD_ARGS := --arch $(ARCH)
ifneq ($(strip $(BUILD_CONFIG)),)
STARRY_BUILD_ARGS += --config $(BUILD_CONFIG)
endif
ifneq ($(strip $(SMP)),)
STARRY_BUILD_ARGS += --smp $(SMP)
endif

STARRY_QEMU_ARGS := $(STARRY_BUILD_ARGS)
ifneq ($(strip $(QEMU_CONFIG)),)
STARRY_QEMU_ARGS += --qemu-config $(QEMU_CONFIG)
endif

XTASK_MAKE_TARGETS := build run debug ci-test
LEGACY_XTASK_KNOBS := LOG DWARF MEMTRACK BLK NET VSOCK MEM ICOUNT DISK_IMG \
	A APP FEATURES APP_FEATURES NO_AXSTD AX_LIB
LEGACY_OVERRIDES := $(strip $(foreach var,$(LEGACY_XTASK_KNOBS),\
	$(if $(filter command line,$(origin $(var))),$(var)=$($(var)))))

ifneq ($(filter $(XTASK_MAKE_TARGETS),$(or $(MAKECMDGOALS),build)),)
ifneq ($(LEGACY_OVERRIDES),)
$(error Legacy Make option(s) $(LEGACY_OVERRIDES) have no xtask equivalent. \
Put build log/features in a Starry build TOML and pass BUILD_CONFIG=<path>; \
put QEMU devices and memory in a QEMU TOML and pass QEMU_CONFIG=<path>)
endif
endif

default: build

rootfs:
	$(XTASK) rootfs --arch $(ARCH)

img:
	@echo -e "\033[33mWARN: The 'img' target is deprecated. Please use 'rootfs' instead.\033[0m"
	@$(MAKE) --no-print-directory rootfs

defconfig:
	@if [ -z "$(BOARD)" ]; then \
		echo "Usage: make defconfig BOARD=<board>"; \
		echo "List boards from the repository root with: cargo xtask starry config ls"; \
		exit 2; \
	fi
	$(XTASK) defconfig "$(BOARD)"

build:
	$(XTASK) build $(STARRY_BUILD_ARGS)

run:
	$(XTASK) qemu $(STARRY_QEMU_ARGS)

debug:
	$(XTASK) qemu $(STARRY_QEMU_ARGS) --debug

justrun disasm:
	@echo "ERROR: 'make $@' has no standalone xtask equivalent." >&2
	@echo "Use 'make run'; inspect the built ELF with rust-objdump." >&2
	@exit 2

ci-test:
	$(XTASK) test qemu --arch $(ARCH)

clean:
	@$(MAKE) -C $(STARRY_DIR)/../arceos clean

# Aliases
rv:
	$(MAKE) ARCH=riscv64 run

la:
	$(MAKE) ARCH=loongarch64 run

sg2002:
	$(XTASK) build --config $(STARRY_DIR)/configs/board/licheerv-nano-sg2002.toml

sg2002-wifi:
	$(XTASK) build --config $(STARRY_DIR)/configs/board/licheerv-nano-sg2002-wifi.toml

.PHONY: default rootfs img defconfig build run justrun debug disasm ci-test clean \
	rv la sg2002 sg2002-wifi
