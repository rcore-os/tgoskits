# Build testcases for rust and c programs

ARCH ?= x86_64
# Whether cross-compiling
TARGET ?= musl

PREFIX := $(ARCH)-linux-$(TARGET)

# Build target for c programs
CC := $(PREFIX)-gcc

CFLAGS := 
ifeq ($(TARGET), musl)
  CFLAGS += -static
endif

all: build

build: build_dir build_c

build_dir:
	@mkdir -p build
	@mkdir -p build/$(ARCH)

build_c:
  # No build for loongarch64
	for app in $(wildcard c/*/*.c); do \
		echo "Building $${app%.c}"; \
		app_name=$$(basename $$(dirname $${app})); \
		$(CC) -o build/$(ARCH)/$${app_name}_c $${app} $(CFLAGS); \
	done

clean:
	@rm -rf build

.PHONY: all build_dir build_c build_rust clean