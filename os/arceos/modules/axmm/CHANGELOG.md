# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.20](https://github.com/rcore-os/tgoskits/compare/ax-mm-v0.5.19...ax-mm-v0.5.20) - 2026-06-22

### Other

- updated the following local packages: ax-hal, ax-alloc

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/ax-mm-v0.5.18...ax-mm-v0.5.19) - 2026-06-12

### Other

- updated the following local packages: ax-hal, ax-alloc

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/ax-mm-v0.5.17...ax-mm-v0.5.18) - 2026-06-11

### Fixed

- *(starry)* support eBPF ringbuf mmap on LoongArch DMW ([#1208](https://github.com/rcore-os/tgoskits/pull/1208))

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/ax-mm-v0.5.16...ax-mm-v0.5.17) - 2026-06-09

### Other

- updated the following local packages: ax-page-table-multiarch, ax-kspin, ax-alloc, ax-hal

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-mm-v0.5.15...ax-mm-v0.5.16) - 2026-06-03

### Other

- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- Refactor workspace structure and update dependencies ([#864](https://github.com/rcore-os/tgoskits/pull/864))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-mm-v0.5.14...ax-mm-v0.5.15) - 2026-05-22

### Other

- updated the following local packages: ax-errno, ax-hal, ax-memory-set, ax-page-table-multiarch, ax-alloc

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-mm-v0.5.13...ax-mm-v0.5.14) - 2026-05-19

### Other

- updated the following local packages: ax-errno, ax-alloc, ax-memory-set, ax-page-table-multiarch, ax-hal

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/ax-mm-v0.5.12...ax-mm-v0.5.13) - 2026-05-15

### Added

- *(mm)* track backend split metadata and generate real /proc maps output ([#306](https://github.com/rcore-os/tgoskits/pull/306))

### Fixed

- *(rockchip-soc)* enable RK3588 USB PHY clocks ([#528](https://github.com/rcore-os/tgoskits/pull/528))

### Other

- *(arceos-modules)* inherit workspace metadata

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-mm-v0.5.11...ax-mm-v0.5.12) - 2026-04-27

### Other

- updated the following local packages: ax-alloc
