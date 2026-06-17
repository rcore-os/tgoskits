# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.3](https://github.com/rcore-os/tgoskits/compare/ax-alloc-v0.8.2...ax-alloc-v0.8.3) - 2026-06-12

### Other

- updated the following local packages: ax-plat

## [0.8.2](https://github.com/rcore-os/tgoskits/compare/ax-alloc-v0.8.1...ax-alloc-v0.8.2) - 2026-06-11

### Fixed

- *(kernel)* harden early allocation and virtio PCI setup

## [0.8.1](https://github.com/rcore-os/tgoskits/compare/ax-alloc-v0.8.0...ax-alloc-v0.8.1) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/ax-alloc-v0.7.2...ax-alloc-v0.8.0) - 2026-06-03

### Added

- *(mm)* add page reclaim for file-backed memory pressure (rebased) ([#1007](https://github.com/rcore-os/tgoskits/pull/1007))

### Fixed

- *(axbacktrace)* harden correctness, optimize allocation, and add per-arch IP adjustment ([#1029](https://github.com/rcore-os/tgoskits/pull/1029))
- *(repo)* normalize allocator and RISC-V dependencies ([#1021](https://github.com/rcore-os/tgoskits/pull/1021))

### Other

- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(starry)* route HAL access through ax-runtime ([#963](https://github.com/rcore-os/tgoskits/pull/963))

## [0.7.2](https://github.com/rcore-os/tgoskits/compare/ax-alloc-v0.7.1...ax-alloc-v0.7.2) - 2026-05-22

### Other

- updated the following local packages: ax-errno, axbacktrace, ax-allocator

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/ax-alloc-v0.7.0...ax-alloc-v0.7.1) - 2026-05-19

### Other

- Refactor Clippy integration and enhance package handling ([#738](https://github.com/rcore-os/tgoskits/pull/738))

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/ax-alloc-v0.6.0...ax-alloc-v0.7.0) - 2026-05-15

### Other

- Implement vfork, getpgrp, and time syscalls with test enhancements ([#409](https://github.com/rcore-os/tgoskits/pull/409))
- *(starry)* drop outdated and unmaintained stuffs ([#353](https://github.com/rcore-os/tgoskits/pull/353))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-alloc-v0.5.11...ax-alloc-v0.6.0) - 2026-04-27

### Other

- *(ax-alloc)* fix percpu slab spelling
