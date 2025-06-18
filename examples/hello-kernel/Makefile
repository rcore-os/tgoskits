ARCH ?= x86_64
APP := hello-kernel

OBJDUMP ?= rust-objdump -d --print-imm-hex --x86-asm-syntax=intel
OBJCOPY ?= rust-objcopy --binary-architecture=$(ARCH)

ifeq ($(ARCH), x86_64)
  TARGET := x86_64-unknown-none
else ifeq ($(ARCH), aarch64)
  TARGET := aarch64-unknown-none-softfloat
else ifeq ($(ARCH), riscv64)
  TARGET := riscv64gc-unknown-none-elf
else ifeq ($(ARCH), loongarch64)
  TARGET := loongarch64-unknown-none-softfloat
else
  $(error "ARCH" must be one of "x86_64", "riscv64", "aarch64" or "loongarch64")
endif

OUT_ELF := $(PWD)/../../target/$(TARGET)/release/$(APP)
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
