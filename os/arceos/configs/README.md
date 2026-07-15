# ArceOS configuration templates

This directory follows the same split as the StarryOS and Axvisor configuration
trees.

- `board/` contains build templates consumed by `cargo xtask arceos defconfig`.
  The `qemu-<arch>.toml` entries select the default dynamic-platform build for
  each supported QEMU architecture.
- `qemu/` contains reusable, architecture-specific QEMU runtime templates.
  They are intended for explicit `--qemu-config` use and deliberately do not
  declare application success markers.

Application directories may retain their own `qemu-<arch>.toml` files when a
case needs a dedicated disk image, host service, timeout, or success pattern.
Those case-specific files remain the default discovered by `arceos qemu`.
