# Makefile for qemu test
BE_TARGET := mips-unknown-linux-gnu
BE_MODE := release
BE_BIN := ../target/$(BE_TARGET)/$(BE_MODE)/rsext4
QEMU_USER := qemu-mips
QEMU_USER_LD := /usr/mips-linux-gnu

CARGO_NIGHTLY := cargo +nightly
BUILD_STD_FLAGS := -Zbuild-std=std,panic_abort


.PHONY: build run clean disk be_build be_run rv64_run
be_build:
	@echo "构建 big-endian usermode..."
	@cd ../ && \
		CARGO_TARGET_MIPS_UNKNOWN_LINUX_GNU_LINKER=mips-linux-gnu-gcc \
		CARGO_TARGET_MIPS_UNKNOWN_LINUX_GNU_AR=mips-linux-gnu-ar \
		$(CARGO_NIGHTLY) build --release --target $(BE_TARGET) $(BUILD_STD_FLAGS)
	@echo "构建完成: $(BE_BIN)"

be_run: be_build
	@echo "启动 big-endian usermode..."
	@$(QEMU_USER) -L $(QEMU_USER_LD) $(BE_BIN)

rv64_run:
	@$(MAKE) -f Makefile.rv64 run

# 清理
clean:
	@cargo clean
	@rm -f disk.img
	@echo "清理完成"
