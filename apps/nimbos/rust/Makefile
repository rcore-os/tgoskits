ARCH ?= x86_64
MODE ?= debug

ifeq ($(ARCH), x86_64)
  target := x86_64-unknown-none
  rust_flags := -C relocation-model=static
else ifeq ($(ARCH), aarch64)
  target := aarch64-unknown-none-softfloat
else ifeq ($(ARCH), riscv64)
  target := riscv64imac-unknown-none-elf
else ifeq ($(ARCH), loongarch64)
  target := loongarch64-unknown-none
else
  $(error Unsupported architecture "$(ARCH)")
endif

target_dir := target/$(target)/$(MODE)
app_dir := src/bin
apps := $(wildcard $(app_dir)/*.rs)
elfs := $(patsubst $(app_dir)/%.rs, $(target_dir)/%, $(apps))
asms := $(patsubst %, %.asm, $(elfs))

build_args := --target $(target) -Zbuild-std=core
ifeq ($(MODE), release)
  build_args += --release
endif

OBJDUMP := rust-objdump -d --print-imm-hex --x86-asm-syntax=intel
OBJCOPY := rust-objcopy --binary-architecture=$(ARCH)

export RUSTFLAGS=$(rust_flags)

all: build

elf:
	cargo build $(build_args)

clippy:
	cargo clippy $(build_args)

$(elfs): elf

$(target_dir)/%.asm: $(target_dir)/%
	$(OBJDUMP) -d $< > $@

disasm: $(asms) $(elfs)

build: elf
	@cp $(elfs) ../build/$(ARCH)

clean:
	@cargo clean

.PHONY: all binary disasm build clean
