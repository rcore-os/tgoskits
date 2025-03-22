# Command to build and run testcases for oscomp

oscomp_binary: ax_root defconfig
	@cp -r $(PWD)/bin/* /root/.cargo/bin
	@make -C $(AX_ROOT) A=$(PWD) EXTRA_CONFIG=$(EXTRA_CONFIG) build
	@if [ "$(ARCH)" = "riscv64" ]; then \
		cp $(OUT_BIN) kernel-rv; \
	else \
		cp $(OUT_ELF) kernel-la; \
	fi

oscomp_build:
	# Build for os competition
	RUSTUP_TOOLCHAIN=nightly-2025-01-18 $(MAKE) oscomp_binary ARCH=riscv64 AX_TESTCASE=oscomp BUS=mmio FEATURES=lwext4_rs 
	RUSTUP_TOOLCHAIN=nightly-2025-01-18 $(MAKE) oscomp_binary ARCH=loongarch64 AX_TESTCASE=oscomp FEATURES=lwext4_rs

oscomp_test: defconfig
	# Test for os competition online
	@./scripts/oscomp_test.sh

IMG_URL := https://github.com/Azure-stars/testsuits-for-oskernel/releases/download/v0.1/sdcard-$(ARCH).img.gz

define load_img
	@if [ ! -f $(PWD)/sdcard-$(ARCH).img ]; then \
		wget $(IMG_URL); \
		gunzip $(PWD)/sdcard-$(ARCH).img.gz; \
	fi
	cp $(PWD)/sdcard-$(ARCH).img $(AX_ROOT)/disk.img
endef

oscomp_run: ax_root defconfig
	$(call load_img)
	$(MAKE) AX_TESTCASE=oscomp BLK=y NET=y FEATURES=fp_simd,lwext4_rs LOG=$(LOG) run

.PHONY: oscomp_binary oscomp_build oscomp_test oscomp_run