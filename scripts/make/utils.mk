# Utility definitions and functions

GREEN_C := \033[92;1m
CYAN_C := \033[96;1m
YELLOW_C := \033[93;1m
GRAY_C := \033[90m
WHITE_C := \033[37m
END_C := \033[0m

ifeq ($(AX_ARCH), x86_64)
  ARCH_STR = amd64
else ifeq ($(AX_ARCH), aarch64)
  ARCH_STR = arm64
else
  ARCH_STR = $(AX_ARCH)
endif

TAR_NAME := ubuntu-base-22.04-base-$(ARCH_STR).tar.gz

define run_cmd
  @printf '$(WHITE_C)$(1)$(END_C) $(GRAY_C)$(2)$(END_C)\n'
  @$(1) $(2)
endef

define make_disk_image_fat32
  @printf "    $(GREEN_C)Creating$(END_C) FAT32 disk image \"$(1)\" ...\n"
  @dd if=/dev/zero of=$(1) bs=1M count=64
  @mkfs.fat -F 32 $(1)
endef

define make_disk_image
  $(if $(filter $(1),fat32), $(call make_disk_image_fat32,$(2)))
endef

define make_guest_ubuntu_ext4
  @printf "    $(GREEN_C)Creating$(END_C) Ubuntu guest image \"$(1)\" ...\n"
  @dd if=/dev/zero of=$(1) bs=1M count=128
  @mkfs.ext4  $(1)
  @mkdir -p tmp/ubuntu_rootfs

  @if [ ! -f "tmp/$(TAR_NAME)" ]; then \
    printf "    $(YELLOW_C)Downloading$(END_C) Ubuntu base image for $(AX_ARCH) ...\n"; \
    wget -O "tmp/$(TAR_NAME)" "http://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/$(TAR_NAME)"; \
  fi

  @printf "    $(GREEN_C)Mounting$(END_C) Ubuntu base image ...\n"
  @sudo mount -t ext4 $(1) tmp/ubuntu_rootfs
  @printf "    $(GREEN_C)Extracting$(END_C) Ubuntu base image ...\n"
  @sudo tar -xzf "tmp/$(TAR_NAME)" -C tmp/ubuntu_rootfs/
  @printf "    $(GREEN_C)Unmounting$(END_C) Ubuntu base image ...\n"
  @sudo umount tmp/ubuntu_rootfs
endef