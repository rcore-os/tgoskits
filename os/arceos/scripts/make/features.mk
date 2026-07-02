# Features resolving.
#
# Inputs:
#   - `FEATURES`: a list of features to be enabled split by spaces or commas.
#     The features can be selected from the crate `ax-feat` or the user library
#     (crate `ax-std` or `ax-libc`).
#   - `APP_FEATURES`: a list of features to be enabled for the Rust app.
#
# Outputs:
#   - `AX_FEAT`: features to be enabled for ArceOS modules (crate `ax-feat`).
#   - `LIB_FEAT`: features to be enabled for the user library (crate `ax-std`, `ax-libc`).
#   - `APP_FEAT`: features to be enabled for the Rust app.

ifeq ($(APP_TYPE),c)
  ax_feat_prefix := ax-feat/
  lib_features := fp-simd irq alloc multitask lockdep fs net fd pipe select epoll
else
  ifeq ($(NO_AXSTD),y)
    ax_feat_prefix := ax-feat/
  else
    ax_feat_prefix := ax-std/
  endif
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

ax_feat :=
lib_feat :=
direct_feat :=

ifneq ($(MYPLAT),)
  ax_feat += myplat
endif

ifeq ($(filter $(LOG),off error warn info debug trace),)
  $(error "LOG" must be one of "off", "error", "warn", "info", "debug", "trace")
endif

ifeq ($(DWARF),y)
  ax_feat += dwarf
endif

ifeq ($(shell test $(SMP) -gt 1; echo $$?),0)
  lib_feat += smp
endif

direct_feat += $(filter %/%,$(FEATURES))
legacy_feat := $(filter-out %/%,$(FEATURES))

ax_feat += $(filter-out $(lib_features),$(legacy_feat))
lib_feat += $(filter $(lib_features),$(legacy_feat))

AX_FEAT := $(strip $(addprefix $(ax_feat_prefix),$(ax_feat)) $(direct_feat))
LIB_FEAT := $(strip $(addprefix $(lib_feat_prefix),$(lib_feat)))
APP_FEAT := $(strip $(shell echo $(APP_FEATURES) | tr ',' ' '))
