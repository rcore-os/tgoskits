# Changelog

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-cpu-v0.5.5...ax-cpu-v0.6.0) - 2026-04-27

### Added

- *(axvisor)* add loongarch64 qemu support and CI ([#242](https://github.com/rcore-os/tgoskits/pull/242))
- *(ax-sync)* add mutex lockdep and fix Starry atomic-context violations ([#271](https://github.com/rcore-os/tgoskits/pull/271))

### Other

- Unifies breakpoint and debug trap handling across archs ([#244](https://github.com/rcore-os/tgoskits/pull/244))

## 0.2.2

### Fixes

* [Fix compile error on riscv when enable `uspace` feature](https://github.com/arceos-org/ax-cpu/pull/12).

## 0.2.1

### Fixes

* [Pad TrapFrame to multiple of 16 bytes for riscv64](https://github.com/arceos-org/ax-cpu/pull/11).

## 0.2.0

### Breaking Changes

* Upgrade `memory_addr` to v0.4.

### New Features

* [Add FP state switch for riscv64](https://github.com/arceos-org/ax-cpu/pull/2).
* [Add hypervisor support for aarch64](https://github.com/arceos-org/ax-cpu/pull/10).

### Other Improvements

* Export `save`/`restore` in FP states for each architecture.
* Improve documentation.

## 0.1.1

### New Features

* Add `init::init_percpu` for x86_64.

### Other Improvements

* Improve documentation.

## 0.1.0

Initial release.
