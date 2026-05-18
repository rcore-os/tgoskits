# Config generation

config_args := \
  configs/defconfig.toml $(PLAT_CONFIG) $(EXTRA_CONFIG) \
  -w 'arch="$(ARCH)"' \
  -w 'platform="$(PLAT_NAME)"' \
  -o "$(OUT_CONFIG)"

ifneq ($(MEM),)
  config_args += -w 'plat.phys-memory-size=$(shell ./scripts/make/strtosz.py $(MEM))'
else
  MEM := $(shell printf "%s" "$(PHYS_MEMORY_SIZE)" | tr -d _ | xargs printf "%dB")
endif

ifneq ($(SMP),)
  config_args += -w 'plat.max-cpu-num=$(SMP)'
else
  SMP := $(PLAT_SMP)
  ifeq ($(SMP),)
    $(error "`plat.max-cpu-num` is not defined in the platform configuration file")
  endif
endif

define defconfig
  $(call run_cmd,mkdir,-p "$(dir $(OUT_CONFIG))")
  $(call run_cmd,$(AXCONFIG),$(config_args))
endef

ifeq ($(wildcard $(OUT_CONFIG)),)
  define oldconfig
    $(call defconfig)
  endef
else
  define oldconfig
    $(if $(filter "$(PLAT_NAME)",$(shell $(AXCONFIG) "$(OUT_CONFIG)" -r platform)),\
         $(call run_cmd,$(AXCONFIG),$(config_args) -c "$(OUT_CONFIG)"),\
         $(error "ARCH" or "MYPLAT" has been changed, please run "make defconfig" again))
  endef
endif
