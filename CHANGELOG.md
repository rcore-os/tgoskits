# Changelog

## 0.2.1

* [Pad TrapFrame to multiple of 16 bytes for riscv64](https://github.com/arceos-org/axcpu/pull/12).

## 0.2.0

### Breaking changes

* Upgrade `memory_addr` to v0.4.

### New Features

* [Add FP state switch for riscv64](https://github.com/arceos-org/axcpu/pull/2).
* [Add hypervisor support for aarch64](https://github.com/arceos-org/axcpu/pull/10).

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
