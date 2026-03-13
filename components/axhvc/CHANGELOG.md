# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-01-24

### Added

- Initial release of axhvc crate
- Define `HyperCallCode` enum with all supported hypercall operations:
  - `HypervisorDisable` - Disable the hypervisor
  - `HyperVisorPrepareDisable` - Prepare to disable the hypervisor
  - `HyperVisorDebug` - Debug hypercall for development
  - `HIVCPublishChannel` - Publish an IVC shared memory channel
  - `HIVCSubscribChannel` - Subscribe to an IVC shared memory channel
  - `HIVCUnPublishChannel` - Unpublish an IVC shared memory channel
  - `HIVCUnSubscribChannel` - Unsubscribe from an IVC shared memory channel
- Define `HyperCallResult` type alias for hypercall return values
- Implement `Debug` trait for `HyperCallCode`
- Full documentation for all public APIs

[Unreleased]: https://github.com/arceos-hypervisor/axhvc/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/arceos-hypervisor/axhvc/releases/tag/v0.1.0
