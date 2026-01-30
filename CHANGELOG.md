# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
