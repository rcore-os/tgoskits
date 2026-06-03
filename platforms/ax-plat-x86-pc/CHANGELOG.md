# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.11](https://github.com/rcore-os/tgoskits/compare/ax-plat-x86-pc-v0.5.10...ax-plat-x86-pc-v0.5.11) - 2026-06-03

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(axvisor)* support x86_64 Linux guest boot (vmx) ([#930](https://github.com/rcore-os/tgoskits/pull/930))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))

### Other

- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- Implement platform-specific IRQ handling and architecture setup ([#979](https://github.com/rcore-os/tgoskits/pull/979))

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/ax-plat-x86-pc-v0.5.9...ax-plat-x86-pc-v0.5.10) - 2026-05-22

### Other

- updated the following local packages: ax-cpu

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/ax-plat-x86-pc-v0.5.8...ax-plat-x86-pc-v0.5.9) - 2026-05-19

### Other

- updated the following local packages: ax-cpu

## [0.5.8](https://github.com/rcore-os/tgoskits/compare/ax-plat-x86-pc-v0.5.7...ax-plat-x86-pc-v0.5.8) - 2026-05-15

### Other

- updated the following local packages: ax-kspin, ax-int-ratio, ax-config-macros, ax-cpu, ax-plat
