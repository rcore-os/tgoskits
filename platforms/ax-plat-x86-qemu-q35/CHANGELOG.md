# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.10](https://github.com/rcore-os/tgoskits/compare/ax-plat-x86-qemu-q35-v0.4.9...ax-plat-x86-qemu-q35-v0.4.10) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))

## [0.4.9](https://github.com/rcore-os/tgoskits/compare/ax-plat-x86-qemu-q35-v0.4.8...ax-plat-x86-qemu-q35-v0.4.9) - 2026-06-03

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(axvisor)* support x86_64 Linux guest boot (vmx) ([#930](https://github.com/rcore-os/tgoskits/pull/930))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))

### Other

- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))

## [0.4.8](https://github.com/rcore-os/tgoskits/compare/axplat-x86-qemu-q35-v0.4.7...axplat-x86-qemu-q35-v0.4.8) - 2026-05-22

### Other

- updated the following local packages: ax-cpu

## [0.4.7](https://github.com/rcore-os/tgoskits/compare/axplat-x86-qemu-q35-v0.4.6...axplat-x86-qemu-q35-v0.4.7) - 2026-05-19

### Other

- updated the following local packages: ax-cpu

## [0.4.6](https://github.com/rcore-os/tgoskits/compare/axplat-x86-qemu-q35-v0.4.5...axplat-x86-qemu-q35-v0.4.6) - 2026-05-15

### Added

- *(console)* add interrupt-driven console input ([#343](https://github.com/rcore-os/tgoskits/pull/343))

### Other

- *(platform)* inherit workspace metadata

## [0.4.5](https://github.com/rcore-os/tgoskits/compare/axplat-x86-qemu-q35-v0.4.4...axplat-x86-qemu-q35-v0.4.5) - 2026-04-27

### Added

- add axconfig.toml for x86-qemu-q35 platform configuration

### Fixed

- update dependencies section for x86_64 target in Cargo.toml
