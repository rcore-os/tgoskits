AX_ROOT ?= $(PWD)/.arceos
AX_TESTCASE ?= nimbos
ARCH ?= x86_64
AX_TESTCASES_LIST=$(shell cat ./apps/$(AX_TESTCASE)/testcase_list | tr '\n' ',')
TARGET ?= x86_64-unknown-none
RUSTDOCFLAGS := -Z unstable-options --enable-index-page -D rustdoc::broken_intra_doc_links -D missing-docs
EXTRA_CONFIG ?= $(PWD)/configs/$(ARCH).toml
ifneq ($(filter $(MAKECMDGOALS),doc_check_missing),) # make doc_check_missing
    export RUSTDOCFLAGS
else ifeq ($(filter $(MAKECMDGOALS),clean user_apps ax_root),) # Not make clean, user_apps, ax_root
    export AX_TESTCASES_LIST
endif

all: build

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

test:
	@./scripts/app_test.sh

defconfig build run justrun debug disasm: ax_root
	@make -C $(AX_ROOT) A=$(PWD) EXTRA_CONFIG=$(EXTRA_CONFIG) $@

clean: ax_root
	@make -C $(AX_ROOT) A=$(PWD) clean
	@cargo clean

doc_check_missing:
	@cargo doc --no-deps --all-features --workspace

.PHONY: all ax_root build run justrun debug disasm clean
