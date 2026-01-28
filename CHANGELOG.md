# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.3.0] - 2026-01-28

### Changed

- Use weak symbols for generated helpers, allowing default trait methods without link failures.
- Reject receivers and generic parameters in interface methods.

### Fixed

- Align license metadata with the Apache-2.0 declaration in the manifest.

## [0.2.0] - 2025-12-20

### Added

- Lightweight declaration-macro companion crate `crate_interface_lite`.
- `gen_caller` option in `def_interface`.
- `namespace` option in `def_interface`, `impl_interface`, and `call_interface`.
- Support for item attributes on interface methods.
- Workspace and MSRV checks in CI.

### Changed

- Bump MSRV to 1.68.
- Hide non-public helper APIs.
- Forbid trait alias implementations (unsound).

### Fixed

- Badge URLs and formatting inconsistencies.

## [0.1.4] - 2025-01-18

### Changed

- Inline extern function calls in `impl_interface`, removing dependency on external thunks.

## [0.1.3] - 2024-07-31

### Added

- Support for access paths in `call_interface`.
- Documentation and CI badges to README.

## [0.1.2] - 2024-07-11

### Fixed

- Stabilize initial API.

## [0.1.1] - 2024-07-11

### Added

- Initial release with core procedural macros, tests, and CI configuration.
