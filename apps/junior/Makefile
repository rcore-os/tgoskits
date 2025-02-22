ARCH ?= riscv64

TESTCASE_URL := https://github.com/Starry-OS/testcases/releases/download/v0.3/$(ARCH)_syscall_testcases.tar.gz

ifeq ($(ARCH), riscv64)
    PLATFORM_NAME := riscv64-qemu-virt
else ifeq ($(ARCH), loongarch64)
    PLATFORM_NAME := loongarch64-qemu-virt
else
	$(error "ARCH" must be "riscv64" or "loongarch64")
endif

BUILD_DIR := build/$(ARCH)
TARGET_TAR := $(BUILD_DIR)/$(ARCH)-syscall-testcases.tar.xz

build:
	@mkdir -p $(BUILD_DIR)
	@if [ -z "$(wildcard $(BUILD_DIR)/*)" ]; then \
		wget $(TESTCASE_URL) -O $(TARGET_TAR); \
		tar -xvf $(TARGET_TAR) -C $(BUILD_DIR); \
	fi

clean:
	@rm -rf $(BUILD_DIR)

.PHONY: build clean
