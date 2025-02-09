ARCH ?= x86_64
MODE ?= release

BUILD_RUST ?= y

ifeq ($(ARCH),loongarch64)
  # No build C for loongarch64
  BUILD_C := n
else
  BUILD_C := y
endif

build_dir := build/$(ARCH)

build: rust c

build_dir:
	@mkdir -p $(build_dir)

rust: build_dir
ifeq ($(BUILD_RUST), y)
	@make -C rust/
endif

c: build_dir
ifeq ($(BUILD_C), y)
	@make -C c/
endif

clean:
	@make -C rust/ clean
	@make -C c/ clean
	@rm -rf $(build_dir)

.PHONY: build build_dir rust c clean
