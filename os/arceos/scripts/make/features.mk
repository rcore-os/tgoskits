# Features resolving.
#
# Inputs:
#   - `FEATURES`: a list of features to be enabled split by spaces or commas.
#     The features can be selected from the user library (crate `ax-std` or
#     `ax-libc`) or by direct crate feature paths.
#   - `APP_FEATURES`: a list of features to be enabled for the Rust app.
#
# Outputs:
#   - `AX_FEATURESURES`: resolved ArceOS user-library or direct crate features.
#   - `LIB_FEAT`: features to be enabled for the user library (crate `ax-std`, `ax-libc`).
#   - `APP_FEAT`: features to be enabled for the Rust app.

ifeq ($(APP_TYPE),c)
  arceos_feature_prefix := ax-libc/
  lib_features := fp-simd irq alloc multitask lockdep fs net fd pipe select epoll
else
  arceos_feature_prefix := ax-std/
  lib_features :=
endif

lib_feat_prefix := $(AX_LIB)/

override FEATURES := $(shell echo $(FEATURES) | tr ',' ' ')

ifeq ($(APP_TYPE), c)
  ifneq ($(wildcard $(APP)/features.txt),)    # check features.txt exists
    override FEATURES += $(shell cat $(APP)/features.txt)
  endif
  ifneq ($(filter fs net pipe select epoll,$(FEATURES)),)
    override FEATURES += fd
  endif
endif

override FEATURES := $(strip $(FEATURES))

arceos_feature :=
lib_feat :=
direct_feat :=

ifeq ($(filter $(LOG),off error warn info debug trace),)
  $(error "LOG" must be one of "off", "error", "warn", "info", "debug", "trace")
endif

ifeq ($(DWARF),y)
  arceos_feature += dwarf
endif

ifeq ($(shell test $(SMP) -gt 1; echo $$?),0)
  lib_feat += smp
endif

direct_feat += $(filter %/%,$(FEATURES))
legacy_feat := $(filter-out %/%,$(FEATURES))

arceos_feature += $(filter-out $(lib_features),$(legacy_feat))
lib_feat += $(filter $(lib_features),$(legacy_feat))

AX_FEATURESURES := $(strip $(addprefix $(arceos_feature_prefix),$(arceos_feature)) $(direct_feat))
LIB_FEAT := $(strip $(addprefix $(lib_feat_prefix),$(lib_feat)))
APP_FEAT := $(strip $(shell echo $(APP_FEATURES) | tr ',' ' '))
