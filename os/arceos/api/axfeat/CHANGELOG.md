# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/ax-feat-v0.5.17...ax-feat-v0.5.18) - 2026-06-05

### Other

- updated the following local packages: ax-driver, ax-hal, ax-fs-ng, ax-net-ng, ax-ipi, ax-task, ax-sync, ax-display, ax-fs, ax-input, ax-net, ax-runtime

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/ax-feat-v0.5.16...ax-feat-v0.5.17) - 2026-06-03

### Added

- *(starryos)* expose K230 KPU device ([#1054](https://github.com/rcore-os/tgoskits/pull/1054))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))
- *(axtask)* add task stack guard page support ([#811](https://github.com/rcore-os/tgoskits/pull/811))

### Other

- *(platform)* remove static aarch64 platforms ([#1074](https://github.com/rcore-os/tgoskits/pull/1074))
- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(axruntime)* remove alloc feature, make it unconditional ([#985](https://github.com/rcore-os/tgoskits/pull/985))
- *(starry)* route HAL access through ax-runtime ([#963](https://github.com/rcore-os/tgoskits/pull/963))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))
- Refactor workspace structure and update dependencies ([#864](https://github.com/rcore-os/tgoskits/pull/864))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-feat-v0.5.15...ax-feat-v0.5.16) - 2026-05-22

### Added

- *(axplat-aarch64)* GICv3 + CNTV backend for Apple HVF native execution ([#511](https://github.com/rcore-os/tgoskits/pull/511))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-feat-v0.5.14...ax-feat-v0.5.15) - 2026-05-19

### Other

- updated the following local packages: ax-alloc, ax-driver, ax-task, ax-hal, ax-sync, ax-display, ax-fs, ax-fs-ng, ax-input, ax-ipi, ax-net, ax-runtime

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-feat-v0.5.13...ax-feat-v0.5.14) - 2026-05-15

### Other

- updated the following local packages: axbacktrace, ax-kspin, ax-alloc, ax-config, ax-hal, ax-sync, ax-fs, ax-fs-ng, ax-log, ax-net, ax-driver, ax-task, ax-display, ax-input, ax-ipi, ax-runtime

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-feat-v0.5.11...ax-feat-v0.5.12) - 2026-04-27

### Other

- updated the following local packages: ax-alloc
