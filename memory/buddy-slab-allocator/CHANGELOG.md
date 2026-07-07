# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Removed the external allocator-interface dependency and restored crate-local `AllocError` / `AllocResult`
- Stopped exporting the generic allocator traits `BaseAllocator`, `ByteAllocator`, `PageAllocator`, and `IdAllocator`
- Public allocator APIs now favor concrete allocator methods directly instead of requiring trait imports
- Removed `alloc_pages_at` because the current buddy/slab architecture does not stably support fixed-address allocation

### Removed
- Removed the external allocator-interface crate from `[dependencies]`

### Migration Notes
- Replace imports of `BaseAllocator`, `ByteAllocator`, `PageAllocator`, and `IdAllocator` with direct method calls on `BuddyPageAllocator`, `CompositePageAllocator`, `SlabByteAllocator`, and `GlobalAllocator`
- `PageAllocatorForSlab` remains available for wiring `SlabByteAllocator` to a page allocator

## [0.2.0] - 2026-03-05

### Added
- Added an external allocator-interface dependency

### Changed
- `AllocError`, `AllocResult`, `BaseAllocator`, `ByteAllocator`, `PageAllocator`, and `IdAllocator` are now re-exported from the external allocator-interface crate instead of being defined locally
- Updated Rust toolchain to `nightly-2026-02-25`
- Benchmarks no longer require `--features bench`; `criterion` and `rand` moved to `[dev-dependencies]`

### Removed
- Removed locally defined allocator trait and error type definitions (now provided by the external allocator-interface crate)
- Removed the deprecated `bench` feature flag

## [0.1.1] - 2026-02-06

### Added
- Comprehensive benchmark suite with criterion
- Benchmark documentation in Chinese (`benches/README_CN.md`)
- Benchmark workflow in CI with Rust 1.93.0 toolchain for compatibility

### Changed
- Increased `MAX_ZONES` from 16 to 32 for more flexible memory region management
- Improved documentation in README with benchmark usage instructions


## [0.1.0] - 2025-01-30

### Added
- Buddy page allocator implementation for page-level allocation
- Slab byte allocator implementation for small object allocation
- Composite page allocator for unified multi-region page allocation
- Global allocator that coordinates page and byte allocators
- Automatic allocation size selection (≤2048 bytes uses slab, >2048 bytes uses page)
- Zero `std` dependency (`#![no_std]`) for embedded/kernel environments
- Optional `log` feature for logging allocation events
- Optional `tracking` feature for memory usage statistics
- `AddrTranslator` trait for virtual-to-physical address translation
- `BaseAllocator`, `ByteAllocator`, `PageAllocator`, and `IdAllocator` traits
- Comprehensive error handling with `AllocError` enum

### Features
- O(1) time complexity for small object allocation
- Buddy algorithm for efficient page allocation with automatic merging
- Support for multiple memory regions
- Flexible page size configuration (const generic)
- Memory fragmentation reduction through slab allocation
- Statistics tracking for debugging and profiling

### Documentation
- Complete API documentation with examples
- README with bilingual (English/Chinese) documentation
- Inline documentation for all public APIs

### Testing
- Integration tests for page allocator
- Integration tests for slab allocator
- Integration tests for global allocator
- DMA32 pages test cases
- Comprehensive edge case coverage

## [Unreleased]: https://github.com/arceos-hypervisor/buddy-slab-allocator/compare/v0.2.0...HEAD

## [0.4.5](https://github.com/rcore-os/tgoskits/compare/buddy-slab-allocator-v0.4.4...buddy-slab-allocator-v0.4.5) - 2026-07-07

### Other

- updated the following local packages: ax-kspin

## [0.4.4](https://github.com/rcore-os/tgoskits/compare/buddy-slab-allocator-v0.4.3...buddy-slab-allocator-v0.4.4) - 2026-07-02

### Other

- updated the following local packages: ax-kspin

## [0.4.3](https://github.com/rcore-os/tgoskits/compare/buddy-slab-allocator-v0.4.2...buddy-slab-allocator-v0.4.3) - 2026-06-27

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))

## [0.4.2](https://github.com/rcore-os/tgoskits/compare/buddy-slab-allocator-v0.4.1...buddy-slab-allocator-v0.4.2) - 2026-06-03

### Other

- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))

## [0.3.1](https://github.com/arceos-hypervisor/buddy-slab-allocator/compare/v0.3.0...v0.3.1) - 2026-04-10

### Other

- Refactor global allocator to support singleton pattern and improve slab management
- Implement per-CPU slab allocator with object-safe interface
- bump version to 0.3.1 and update changelog for enhancements
- enhance region layout handling and alignment in allocator methods

## [0.3.1](https://github.com/arceos-hypervisor/buddy-slab-allocator/compare/v0.3.0...v0.3.1) - 2026-04-09

### Other

- enhance region layout handling and alignment in allocator methods

## [0.3.0](https://github.com/arceos-hypervisor/buddy-slab-allocator/compare/v0.2.0...v0.3.0) - 2026-04-09

### Added

- enhance slab allocation by reclaiming full slabs with remote frees and add integration tests for cross-CPU deallocation
- implement Default trait for BuddyAllocator, GlobalAllocator, and SlabAllocator
- remove alloc_pages_at method from buddy allocator and related components; simplify allocation logic
- refactor allocator interfaces and remove deprecated dependencies; update documentation and examples
- enhance logging support by integrating log crate and updating documentation

### Other

- add unsafe blocks for improved safety checks in allocator methods
- update changelog for version 0.2.1 and modify Cargo.toml for version bump to 0.3.0
- format code for better readability in common.rs
- streamline region initialization by introducing SectionInitSpec for better clarity and maintainability
- Refactor GlobalAllocator to support multiple managed sections
- Refactor stress tests for allocator stability
- update allocator initialization to use a mutable slice instead of separate start and size parameters
- Refactor slab allocator benchmarks and improve global allocator initialization
- Refactor integration and stress tests for buddy-slab-allocator
- Refactor integration and stress tests for allocator
- Refactor benchmarks and remove stability tests
- update workflows and dependencies; migrate to actions/checkout@v6 and rand v0.10

## [0.2.1](https://github.com/arceos-hypervisor/buddy-slab-allocator/compare/v0.2.0...v0.2.1) - 2026-04-09

### Added

- enhance slab allocation by reclaiming full slabs with remote frees and add integration tests for cross-CPU deallocation
- implement Default trait for BuddyAllocator, GlobalAllocator, and SlabAllocator
- remove alloc_pages_at method from buddy allocator and related components; simplify allocation logic
- refactor allocator interfaces and remove deprecated dependencies; update documentation and examples
- enhance logging support by integrating log crate and updating documentation

### Other

- format code for better readability in common.rs
- streamline region initialization by introducing SectionInitSpec for better clarity and maintainability
- Refactor GlobalAllocator to support multiple managed sections
- Refactor stress tests for allocator stability
- update allocator initialization to use a mutable slice instead of separate start and size parameters
- Refactor slab allocator benchmarks and improve global allocator initialization
- Refactor integration and stress tests for buddy-slab-allocator
- Refactor integration and stress tests for allocator
- Refactor benchmarks and remove stability tests
- update workflows and dependencies; migrate to actions/checkout@v6 and rand v0.10
## [0.2.0]: https://github.com/arceos-hypervisor/buddy-slab-allocator/compare/v0.1.1...v0.2.0
## [0.1.1]: https://github.com/arceos-hypervisor/buddy-slab-allocator/compare/v0.1.0...v0.1.1
## [0.1.0]: https://github.com/arceos-hypervisor/buddy-slab-allocator/releases/tag/v0.1.0
