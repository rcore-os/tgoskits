# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-01-24

### Added

- Added `api_def` and `api_impl` procedural macros for defining and implementing APIs.
- Added `arch` module with architecture-specific APIs for AArch64 GIC operations.
- Added `host` module with host system APIs.
- Added `memory` module with memory allocation and address translation APIs.
- Added `time` module with time and timer APIs.
- Added `vmm` module with virtual machine management APIs.
- Added `PhysFrame` type alias for automatic frame deallocation.
- Added comprehensive documentation and examples in crate-level docs.
- Added docs.rs configuration for multi-target documentation.

### Changed

- Improved Cargo.toml with complete metadata fields.
- Enhanced CI configuration with documentation checks.

## [0.1.0] - Initial Release

### Added

- Initial implementation of the axvisor_api crate.
- Basic API definition and implementation framework using `crate_interface`.

[Unreleased]: https://github.com/arceos-hypervisor/axvisor_api/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/arceos-hypervisor/axvisor_api/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/arceos-hypervisor/axvisor_api/releases/tag/v0.1.0
