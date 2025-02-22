AX_ROOT ?= $(PWD)/.arceos
AX_TESTCASE ?= nimbos
ARCH ?= x86_64
AX_TESTCASES_LIST=$(shell cat ./apps/$(AX_TESTCASE)/testcase_list | tr '\n' ',')
TARGET ?= x86_64-unknown-none
FEATURES ?= fp_simd

RUSTDOCFLAGS := -Z unstable-options --enable-index-page -D rustdoc::broken_intra_doc_links -D missing-docs
EXTRA_CONFIG ?= $(PWD)/configs/$(ARCH).toml
ifneq ($(filter $(MAKECMDGOALS),doc_check_missing),) # make doc_check_missing
    export RUSTDOCFLAGS
else ifeq ($(filter $(MAKECMDGOALS),clean user_apps ax_root),) # Not make clean, user_apps, ax_root
    export AX_TESTCASES_LIST
endif

DIR := $(shell basename $(PWD))
OUT_ELF := $(DIR)_$(ARCH)-qemu-virt.elf
OUT_BIN := $(DIR)_$(ARCH)-qemu-virt.bin

all:
	# Build for os competition
	RUSTUP_TOOLCHAIN=nightly-2025-01-18 $(MAKE) test_build ARCH=riscv64 AX_TESTCASE=oscomp BUS=mmio  FEATURES=lwext4_rs 
	# If loongarch64-linux-musl-cc is not found, please create a symbolic link to loongarch64-linux-musl-gcc
	@if [ ! -f /opt/musl-loongarch64-1.2.2/bin/loongarch64-linux-musl-cc ]; then \
		echo "loongarch64-linux-musl-cc not found, creating symbolic link to loongarch64-linux-musl-gcc"; \
		cd /opt/musl-loongarch64-1.2.2/bin/ && ln -s loongarch64-linux-musl-gcc loongarch64-linux-musl-cc; \
	fi
	RUSTUP_TOOLCHAIN=nightly-2025-01-18 $(MAKE) test_build ARCH=loongarch64 AX_TESTCASE=oscomp FEATURES=lwext4_rs

TARGET_LIST := x86_64-unknown-none riscv64gc-unknown-none-elf aarch64-unknown-none
ifeq ($(filter $(TARGET),$(TARGET_LIST)),)
$(error TARGET must be one of $(TARGET_LIST))
endif

# export dummy config for clippy
clippy:
	@AX_CONFIG_PATH=$(PWD)/configs/dummy.toml cargo clippy --target $(TARGET) --all-features -- -D warnings -A clippy::new_without_default	

ax_root:
	@./scripts/set_ax_root.sh $(AX_ROOT)
	@make -C $(AX_ROOT) disk_img

user_apps:
	@make -C ./apps/$(AX_TESTCASE) ARCH=$(ARCH) build
	@./build_img.sh -a $(ARCH) -file ./apps/$(AX_TESTCASE)/build/$(ARCH) -s 20
	@mv ./disk.img $(AX_ROOT)/disk.img

test:
	@./scripts/app_test.sh

test_build: ax_root
	@cp -r $(PWD)/bin/* /root/.cargo/bin
	@rustup override set nightly-2025-01-18
	$(MAKE) defconfig EXTRA_CONFIG=$(EXTRA_CONFIG) ARCH=$(ARCH)
	@make -C $(AX_ROOT) A=$(PWD) EXTRA_CONFIG=$(EXTRA_CONFIG) BLK=y NET=y build
	@if [ "$(ARCH)" = "riscv64" ]; then \
		cp $(OUT_BIN) kernel-rv; \
	else \
		cp $(OUT_ELF) kernel-la; \
	fi
	
defconfig build run justrun debug disasm: ax_root
	@make -C $(AX_ROOT) A=$(PWD) EXTRA_CONFIG=$(EXTRA_CONFIG) BLK=y NET=y $@

clean: ax_root
	@make -C $(AX_ROOT) A=$(PWD) ARCH=$(ARCH) clean
	@cargo clean

doc_check_missing:
	@cargo doc --no-deps --all-features --workspace

.PHONY: all ax_root build run justrun debug disasm clean test_build
