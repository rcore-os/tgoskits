AX_ROOT ?= $(PWD)/.arceos
AX_TESTCASE ?= nimbos
ARCH ?= x86_64
AX_TESTCASES_LIST=$(shell cat ./apps/$(AX_TESTCASE)/testcase_list | tr '\n' ',')

RUSTDOCFLAGS := -Z unstable-options --enable-index-page -D rustdoc::broken_intra_doc_links -D missing-docs

ifneq ($(filter $(MAKECMDGOALS),doc_check_missing),) # make doc_check_missing
    export RUSTDOCFLAGS
else ifeq ($(filter $(MAKECMDGOALS),clean user_apps ax_root),) # Not make clean, user_apps, ax_root
    export AX_TESTCASES_LIST
endif

all: build

ax_root:
	@./scripts/set_ax_root.sh $(AX_ROOT)

user_apps:
	@make -C ./apps/$(AX_TESTCASE) ARCH=$(ARCH) build

test:
	@./scripts/app_test.sh

build run justrun debug disasm: ax_root
	@make -C $(AX_ROOT) A=$(PWD) $@

clean: ax_root
	@make -C $(AX_ROOT) A=$(PWD) clean
	@cargo clean

doc_check_missing:
	@cargo doc --no-deps --all-features --workspace

.PHONY: all ax_root build run justrun debug disasm clean
