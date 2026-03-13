# Cargo features and build args

ifeq ($(V),1)
  verbose := -v
else ifeq ($(V),2)
  verbose := -vv
else
  verbose :=
endif

build_args-release := --release

build_args := \
  -Z unstable-options \
  --target $(TARGET) \
  --target-dir $(TARGET_DIR) \
  $(build_args-$(MODE)) \
  $(verbose)

RUSTFLAGS_LINK_ARGS := -C link-arg=-T$(LD_SCRIPT) -C link-arg=-no-pie -C link-arg=-znostart-stop-gc
RUSTDOCFLAGS := -Z unstable-options --enable-index-page -D rustdoc::broken_intra_doc_links

ifeq ($(MAKECMDGOALS), doc_check_missing)
  RUSTDOCFLAGS += -D missing-docs
endif

define cargo_build
  $(call run_cmd,cargo -C $(1) build,$(build_args) --features "$(strip $(2))")
endef
