# Build testcases for rust and c programs

ARCH ?= x86_64
# Whether cross-compiling
TARGET ?= musl

# Build target for c programs
CC := $(ARCH)-linux-$(TARGET)-gcc

# Build target for rust programs
ifeq ($(TARGET),musl)
  CFLAGS := -static -no-pie
  ifeq ($(ARCH),x86_64)
    RUST_TARGET := x86_64-unknown-linux-musl
    RUSTFLAGS := ""
  else ifeq ($(ARCH),aarch64)
    RUST_TARGET := aarch64-unknown-linux-musl
    RUSTFLAGS := "-C linker=aarch64-linux-musl-ld"
  else ifeq ($(ARCH),riscv64)
    $(warning "Warn: Rust musl target not supported for riscv64")
    RUST_TARGET := ""
    RUSTFLAGS := ""
  else ifeq ($(ARCH),loongarch64)
    RUST_TARGET := loongarch64-unknown-linux-musl
    RUSTFLAGS := "-C linker=loongarch-linux-musl-ld"
  else
    $(error "Unknown ARCH")
  endif
else ifeq ($(TARGET),gnu)
  CFLAGS :=
  ifeq ($(ARCH),x86_64)
    RUST_TARGET := x86_64-unknown-linux-gnu
  else ifeq ($(ARCH),aarch64)
    RUST_TARGET := aarch64-unknown-linux-gnu
  else ifeq ($(ARCH),riscv64)
    RUST_TARGET := riscv64gc-unknown-linux-gnu
  else ifeq ($(ARCH),loongarch64)
    RUST_TARGET := loongarch64-unknown-linux-gnu
  else
    $(error "Unknown ARCH")
  endif
else
  $(error "Unknown TARGET")
endif

all: build

build: build_dir build_c build_rust

build_dir:
	@mkdir -p build
	@mkdir -p build/$(ARCH)

build_c:
  # No build for loongarch64
	if [ "$(ARCH)" != "loongarch64" ]; then \
		for app in $(wildcard c/*/*.c); do \
			echo "Building $${app%.c}"; \
			app_name=$$(basename $$(dirname $${app})); \
			$(CC) -o build/$(ARCH)/$${app_name}_c $${app} $(CFLAGS); \
		done \
	fi

build_rust:
	if [ -n $(RUST_TARGET) ]; then \
		for app in $(shell find rust -name Cargo.toml); do \
			echo "Building $$(dirname $${app})"; \
			app_name=$$(basename $$(dirname $${app})); \
			RUSTFLAGS=$(RUSTFLAGS) cargo build --release --target $(RUST_TARGET) --manifest-path $${app} ; \
			cp $$(dirname $${app})/target/$(RUST_TARGET)/release/$${app_name} build/$(ARCH)/$${app_name}_rust ; \
		done \
	fi

clean:
	@rm -rf build
	@for app in $(shell find rust -name Cargo.toml); do \
		app_name=$$(basename $$(dirname $${app})); \
		cargo clean --manifest-path $${app} ; \
	done

.PHONY: all build_dir build_c build_rust clean