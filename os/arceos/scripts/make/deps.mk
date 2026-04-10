# Necessary dependencies for the build system

# Tool to parse information about the target package
ifeq ($(shell cargo axplat --version 2>/dev/null),)
  $(info Installing cargo-axplat...)
  $(shell cargo install cargo-axplat)
endif

# Tool to generate platform configuration files
ifeq ($(shell ax-config-gen --version 2>/dev/null),)
  $(info Installing ax-config-gen...)
  $(shell cargo install ax-config-gen)
endif

# Cargo binutils
ifeq ($(shell cargo install --list | grep cargo-binutils),)
  $(info Installing cargo-binutils...)
  $(shell cargo install cargo-binutils)
endif
