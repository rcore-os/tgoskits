AX_ROOT ?= $(PWD)/.arceos
AX_TESTCASE ?= nimbos
ARCH ?= x86_64

export AX_TESTCASES_LIST=$(shell cat ./apps/$(AX_TESTCASE)/testcase_list | tr '\n' ',')

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

.PHONY: all ax_root build run justrun debug disasm clean
