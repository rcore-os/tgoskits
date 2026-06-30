# GNU Make language feature carpet (self-contained, deterministic) for StarryOS.
include config.mk
NAMES   := gamma alpha beta delta
SORTED  := $(sort $(NAMES))
COUNT   := $(words $(NAMES))
UPPER    = $(NAMES)
FLAGS   := -a
FLAGS   += -b -c
OUT     := $(addprefix $(PREFIX)-,$(SORTED))
FILTERED := $(filter %a,$(NAMES))
SUBST   := $(patsubst %a,%A,$(NAMES))
.PHONY: all summary $(OUT)
all: $(OUT) summary
$(OUT):
	@echo "  make: built $@"
ifeq ($(COUNT),4)
COND := four
else
COND := other
endif
ifdef PREFIX
HASPFX := yes
else
HASPFX := no
endif
summary: $(OUT)
	@echo "MK_SORTED=$(SORTED)"
	@echo "MK_COUNT=$(COUNT)"
	@echo "MK_FLAGS=$(FLAGS)"
	@echo "MK_FILTER=$(FILTERED)"
	@echo "MK_PATSUBST=$(SUBST)"
	@echo "MK_FOREACH=$(foreach n,1 2 3,x$(n))"
	@echo "MK_COND=$(COND)"
	@echo "MK_HASPFX=$(HASPFX)"
	@echo "MK_INCLUDE_TOOLSET=$(TOOLSET)"
	@echo "MAKE_LANG_OK"
