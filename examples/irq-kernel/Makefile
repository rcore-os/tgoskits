ARCH ?= x86_64
APP := irq-kernel

OBJDUMP ?= rust-objdump -d --print-imm-hex --x86-asm-syntax=intel
OBJCOPY ?= rust-objcopy --binary-architecture=$(ARCH)

ifeq ($(ARCH), x86_64)
  TARGET := x86_64-unknown-none
  PLAT_PACKAGE := axplat-x86-pc
else ifeq ($(ARCH), aarch64)
  TARGET := aarch64-unknown-none-softfloat
  PLAT_PACKAGE := axplat-aarch64-qemu-virt
else ifeq ($(ARCH), riscv64)
  TARGET := riscv64gc-unknown-none-elf
  PLAT_PACKAGE := axplat-riscv64-qemu-virt
else ifeq ($(ARCH), loongarch64)
  TARGET := loongarch64-unknown-none-softfloat
  PLAT_PACKAGE := axplat-loongarch64-qemu-virt
else
  $(error "ARCH" must be one of "x86_64", "riscv64", "aarch64" or "loongarch64")
endif

# Tool to parse information about the target package
ifeq ($(shell cargo axplat --version 2>/dev/null),)
  $(info "Installing cargo-axplat...")
  $(shell cargo install cargo-axplat)
endif

PLAT_CONFIG := $(shell cargo axplat info -c $(PLAT_PACKAGE) 2>/dev/null)

export AX_CONFIG_PATH := $(PLAT_CONFIG)

OUT_ELF := $(CURDIR)/../../target/$(TARGET)/release/$(APP)
OUT_BIN := $(OUT_ELF).bin

qemu_args-x86_64 := \
  -machine q35 \
  -kernel $(OUT_ELF)

qemu_args-riscv64 := \
  -machine virt \
  -bios default \
  -kernel $(OUT_BIN)

qemu_args-aarch64 := \
  -cpu cortex-a72 \
  -machine virt \
  -kernel $(OUT_BIN)

qemu_args-loongarch64 := \
  -machine virt \
  -m 1G \
  -kernel $(OUT_ELF)

all: build

build:
	cargo build -p $(APP) --target $(TARGET) --release

$(OUT_BIN): build
	$(OBJCOPY) --strip-all -O binary $(OUT_ELF) $(OUT_BIN)

run: $(OUT_BIN)
	qemu-system-$(ARCH) $(qemu_args-$(ARCH)) -nographic

disasm:
	$(OBJDUMP) $(OUT_ELF) | less

clippy:
	cargo clippy -p $(APP) --target $(TARGET)

clean:
	cargo clean
