# Architecture and platform resolving

inspect_platform = $(shell $(AXPLAT_INSPECT) --manifest-dir "$(CARGO_MANIFEST_DIR)" --package $(PLAT_PACKAGE))
inspect_value = $(strip $(patsubst $(1)=%,%,$(filter $(1)=%,$(platform_info))))

config_value = $(strip $(shell $(AXCONFIG) "$(PLAT_CONFIG)" -r $(1)))
config_string_value = $(patsubst "%",%,$(call config_value,$(1)))
platform_package_aliases = $(strip $(1) $(patsubst axplat-%,ax-plat-%,$(1)) $(patsubst ax-plat-%,axplat-%,$(1)))

define validate_config
  $(if $(strip $(PLAT_PACKAGE)),,$(error PLAT_CONFIG=$(PLAT_CONFIG) is not a valid platform configuration file)) \
  $(if $(filter $(EXPECTED_PLAT_PACKAGE),$(PLAT_PACKAGE)),,\
    $(error `PLAT_PACKAGE` field mismatch: expected $(EXPECTED_PLAT_PACKAGE), got $(PLAT_PACKAGE)))
endef

define default_platform_package
  $(if $(filter x86_64,$(ARCH)),ax-plat-x86-pc,\
    $(if $(filter aarch64,$(ARCH)),$(error AArch64 no longer has a repo-owned static default platform; use cargo xtask for plat-dyn builds or pass MYPLAT/PLAT_CONFIG explicitly),\
      $(if $(filter riscv64,$(ARCH)),ax-plat-riscv64-qemu-virt,\
        $(if $(filter loongarch64,$(ARCH)),ax-plat-loongarch64-qemu-virt,\
          $(error "ARCH" must be one of "x86_64", "riscv64", "aarch64" or "loongarch64")))))
endef

ifneq ($(wildcard $(PLAT_CONFIG)),)
  PLAT_PACKAGE := $(call config_string_value,package)
  EXPECTED_PLAT_PACKAGE := $(if $(MYPLAT),$(call platform_package_aliases,$(MYPLAT)),$(PLAT_PACKAGE))
  PLAT_NAME := $(call config_string_value,platform)
  PLAT_ARCH := $(call config_string_value,arch)
  PLAT_SMP := $(call config_value,plat.max-cpu-num)
  PHYS_MEMORY_SIZE := $(call config_value,plat.phys-memory-size)
  $(call validate_config)

  _arch := $(PLAT_ARCH)
  ifeq ($(origin ARCH),command line)
    ifneq ($(ARCH),$(_arch))
      $(error "ARCH=$(ARCH)" is not compatible with "PLAT_CONFIG=$(PLAT_CONFIG)")
    endif
  endif
  ARCH := $(_arch)
else ifeq ($(MYPLAT),)
  # `MYPLAT` is not specified, use the default platform for each architecture
  PLAT_PACKAGE := $(strip $(call default_platform_package))
  EXPECTED_PLAT_PACKAGE := $(PLAT_PACKAGE)
  $(if $(strip $(AXPLAT_INSPECT)),,$(error AXPLAT_INSPECT is required when PLAT_CONFIG is not set))
  platform_info := $(call inspect_platform)
  PLAT_CONFIG := $(call inspect_value,PLAT_CONFIG)
  PLAT_PACKAGE := $(call inspect_value,PLAT_PACKAGE)
  PLAT_NAME := $(call inspect_value,PLAT_NAME)
  PLAT_ARCH := $(call inspect_value,PLAT_ARCH)
  PLAT_SMP := $(call inspect_value,PLAT_SMP)
  PHYS_MEMORY_SIZE := $(call inspect_value,PHYS_MEMORY_SIZE)
  # We don't need to check whether `PLAT_CONFIG` is valid here, as the `PLAT_PACKAGE`
  # is a valid pacakage.

  $(call validate_config)
else
  # `MYPLAT` is specified, treat it as a package name
  PLAT_PACKAGE := $(MYPLAT)
  EXPECTED_PLAT_PACKAGE := $(call platform_package_aliases,$(PLAT_PACKAGE))
  $(if $(strip $(AXPLAT_INSPECT)),,$(error AXPLAT_INSPECT is required when PLAT_CONFIG is not set))
  platform_info := $(call inspect_platform)
  PLAT_CONFIG := $(call inspect_value,PLAT_CONFIG)
  PLAT_PACKAGE := $(call inspect_value,PLAT_PACKAGE)
  PLAT_NAME := $(call inspect_value,PLAT_NAME)
  PLAT_ARCH := $(call inspect_value,PLAT_ARCH)
  PLAT_SMP := $(call inspect_value,PLAT_SMP)
  PHYS_MEMORY_SIZE := $(call inspect_value,PHYS_MEMORY_SIZE)
  ifeq ($(wildcard $(PLAT_CONFIG)),)
    $(error "MYPLAT=$(MYPLAT) is not a valid platform package name")
  endif
  $(call validate_config)

  # Read the architecture name from the configuration file
  _arch := $(PLAT_ARCH)
  ifeq ($(origin ARCH),command line)
    ifneq ($(ARCH),$(_arch))
      $(error "ARCH=$(ARCH)" is not compatible with "MYPLAT=$(MYPLAT)")
    endif
  endif
  ARCH := $(_arch)
endif
