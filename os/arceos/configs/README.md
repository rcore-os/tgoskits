# ArceOS configuration templates

This directory follows the same split as the StarryOS and Axvisor configuration
trees.

- `board/` contains build templates consumed by `cargo xtask arceos defconfig`.
  The `qemu-<arch>.toml` entries select the default dynamic-platform Hello World build
  for each supported QEMU architecture when `arceos qemu` has no package or
  build-config selector.
- `qemu/` contains reusable, architecture-specific QEMU runtime templates.
  They are intended for explicit `--qemu-config` use and deliberately do not
  declare application success markers.

Application directories may retain their own `qemu-<arch>.toml` files when a
case needs a dedicated disk image, host service, timeout, or success pattern.
Those case-specific files remain the default discovered by `arceos qemu`.
