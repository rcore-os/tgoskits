# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/riscv_vcpu-v0.5.11...riscv_vcpu-v0.5.12) - 2026-06-11

### Fixed

- fix typos in code and comments across the codebase ([#1206](https://github.com/rcore-os/tgoskits/pull/1206))

## [0.5.11](https://github.com/rcore-os/tgoskits/compare/riscv_vcpu-v0.5.10...riscv_vcpu-v0.5.11) - 2026-06-09

### Other

- Refactor Axvisor to unify ArceOS API and improve modularity ([#1019](https://github.com/rcore-os/tgoskits/pull/1019))

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/riscv_vcpu-v0.5.9...riscv_vcpu-v0.5.10) - 2026-06-03

### Added

- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))

### Fixed

- *(repo)* normalize allocator and RISC-V dependencies ([#1021](https://github.com/rcore-os/tgoskits/pull/1021))

### Other

- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/riscv_vcpu-v0.5.8...riscv_vcpu-v0.5.9) - 2026-05-22

### Fixed

- *(axvisor)* recover riscv guest memory faults ([#788](https://github.com/rcore-os/tgoskits/pull/788))

## [0.5.8](https://github.com/rcore-os/tgoskits/compare/riscv_vcpu-v0.5.7...riscv_vcpu-v0.5.8) - 2026-05-19

### Other

- Refactor Clippy integration and enhance package handling ([#738](https://github.com/rcore-os/tgoskits/pull/738))

## [0.5.7](https://github.com/rcore-os/tgoskits/compare/riscv_vcpu-v0.5.6...riscv_vcpu-v0.5.7) - 2026-05-15

### Added

- *(riscv64)* add virtual PMU support and enhance with performance counters ([#405](https://github.com/rcore-os/tgoskits/pull/405))

### Fixed

- reorder imports in vpmu.rs for clarity ([#634](https://github.com/rcore-os/tgoskits/pull/634))

### Other

- bump crate versions and dependencies ([#630](https://github.com/rcore-os/tgoskits/pull/630))
- *(riscv-vcpu)* inherit workspace metadata

## [0.5.6](https://github.com/rcore-os/tgoskits/compare/riscv_vcpu-v0.5.5...riscv_vcpu-v0.5.6) - 2026-04-27

### Other

- *(axvisor)* add Linux guest support to the AxVisor riscv64 QEMU test ([#351](https://github.com/rcore-os/tgoskits/pull/351))
- *(riscv_vcpu)* gate riscv64-only sources
