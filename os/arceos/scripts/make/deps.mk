# Necessary dependencies for the build system

# Tool to generate platform configuration files
ifeq ($(shell $(AXCONFIG) --version 2>/dev/null),)
  $(info Installing ax-config-gen...)
  $(shell cargo install ax-config-gen)
endif

# Cargo binutils
ifeq ($(shell cargo install --list | grep cargo-binutils),)
  $(info Installing cargo-binutils...)
  $(shell cargo install cargo-binutils)
endif
