# Build Options
export ARCH := riscv64
export LOG := warn
export DWARF := y
export MEMTRACK := n

# QEMU Options
export BLK := y
export NET := y
export VSOCK := n
export MEM := 1G
export ICOUNT := n
export DISK_IMG := rootfs-$(ARCH).img

# Generated Options
export A := $(CURDIR)/starryos
export NO_AXSTD := y
export AX_LIB := ax-std
export APP_FEATURES := qemu

ifeq ($(MEMTRACK), y)
	APP_FEATURES += starry-api/memtrack
endif

default: build

ROOTFS_URL = https://github.com/Starry-OS/rootfs/releases/download/20260214
ROOTFS_IMG = rootfs-$(ARCH).img

rootfs:
	@if [ ! -f $(ROOTFS_IMG) ]; then \
		echo "Image not found, downloading..."; \
		curl -f -L $(ROOTFS_URL)/$(ROOTFS_IMG).xz -O; \
		xz -d $(ROOTFS_IMG).xz; \
	fi
	@ln -sf $(CURDIR)/$(ROOTFS_IMG) $(CURDIR)/../arceos/rootfs-$(ARCH).img

img:
	@echo -e "\033[33mWARN: The 'img' target is deprecated. Please use 'rootfs' instead.\033[0m"
	@$(MAKE) --no-print-directory rootfs

defconfig justrun clean:
	@$(MAKE) -C $(CURDIR)/../arceos $@

build run debug disasm: rootfs defconfig
	@$(MAKE) -C $(CURDIR)/../arceos $@

ci-test:
	./scripts/ci-test.py $(ARCH)

# Aliases
rv:
	$(MAKE) ARCH=riscv64 run

la:
	$(MAKE) ARCH=loongarch64 run

sg2002:
	cargo xtask starry build --config $(CURDIR)/configs/board/licheerv-nano-sg2002.toml

sg2002-wifi:
	cargo xtask starry build --config $(CURDIR)/configs/board/licheerv-nano-sg2002-wifi.toml

.PHONY: build run justrun debug disasm clean sg2002 sg2002-wifi
