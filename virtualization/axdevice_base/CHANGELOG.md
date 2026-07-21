# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/axdevice_base-v0.6.1...axdevice_base-v0.6.2) - 2026-07-21

### Added

- *(axdevice)* register exclusive IRQ line resources ([#1630](https://github.com/rcore-os/tgoskits/pull/1630))

### Other

- *(axdevice)* replace errno contracts ([#1595](https://github.com/rcore-os/tgoskits/pull/1595))
- *(axvm-types)* introduce backend errors ([#1591](https://github.com/rcore-os/tgoskits/pull/1591))

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/axdevice_base-v0.6.0...axdevice_base-v0.6.1) - 2026-07-07

### Other

- updated the following local packages: axvm-types

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/axdevice_base-v0.5.1...axdevice_base-v0.6.0) - 2026-07-02

### Other

- *(axvm)* decouple vcpu backends ([#1467](https://github.com/rcore-os/tgoskits/pull/1467))

## [0.5.1](https://github.com/rcore-os/tgoskits/compare/axdevice_base-v0.5.0...axdevice_base-v0.5.1) - 2026-06-27

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))

### Other

- *(axdevice)* unify Device model with indexed dispatch and conflict detect ([#1335](https://github.com/rcore-os/tgoskits/pull/1335))

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/axdevice_base-v0.4.14...axdevice_base-v0.5.0) - 2026-06-22

### Other

- Issue 595 device foundation ([#1258](https://github.com/rcore-os/tgoskits/pull/1258))

## [0.4.14](https://github.com/rcore-os/tgoskits/compare/axdevice_base-v0.4.13...axdevice_base-v0.4.14) - 2026-06-09

### Other

- updated the following local packages: axvm-types

### Fixed

- publish the device address and access-width re-exports required by virtualization crates

## [0.4.12](https://github.com/rcore-os/tgoskits/compare/axdevice_base-v0.4.11...axdevice_base-v0.4.12) - 2026-06-03

### Other

- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.4.11](https://github.com/rcore-os/tgoskits/compare/axdevice_base-v0.4.10...axdevice_base-v0.4.11) - 2026-05-22

### Other

- updated the following local packages: ax-errno, axaddrspace, axvmconfig

## [0.4.10](https://github.com/rcore-os/tgoskits/compare/axdevice_base-v0.4.9...axdevice_base-v0.4.10) - 2026-05-19

### Other

- updated the following local packages: ax-errno, axaddrspace, axvmconfig

## [0.4.9](https://github.com/rcore-os/tgoskits/compare/axdevice_base-v0.4.8...axdevice_base-v0.4.9) - 2026-05-18

### Other

- updated the following local packages: axaddrspace

## [0.4.8](https://github.com/rcore-os/tgoskits/compare/axdevice_base-v0.4.7...axdevice_base-v0.4.8) - 2026-05-15

### Other

- *(axdevice-base)* inherit workspace dependencies

## [0.1.0] - 2026-01-24

### Added

- Initial release of `axdevice_base` crate.
- `BaseDeviceOps` trait: Core interface for all emulated devices.
- `EmulatedDeviceConfig`: Configuration structure for device initialization.
- `EmuDeviceType`: Re-exported device type enumeration from `axvmconfig`.
- `map_device_of_type`: Helper function for runtime device type checking and casting.
- Trait aliases for common device types:
  - `BaseMmioDeviceOps`: For MMIO (Memory-Mapped I/O) devices.
  - `BaseSysRegDeviceOps`: For system register devices (ARM).
  - `BasePortDeviceOps`: For port I/O devices (x86).
- Support for multiple architectures: x86_64, AArch64, RISC-V64.
- `no_std` compatible design.

[Unreleased]: https://github.com/arceos-hypervisor/axdevice_base/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/arceos-hypervisor/axdevice_base/releases/tag/v0.1.0
