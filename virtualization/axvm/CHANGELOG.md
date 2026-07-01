# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.17...axvm-v0.5.18) - 2026-07-01

### Added

- *(axvisor)* support LoongArch Linux guest on QEMU ([#1207](https://github.com/rcore-os/tgoskits/pull/1207))

### Fixed

- *(axvisor)* gate x86 host fs passthrough prepare
- *(axvm)* resolve LoongArch passthrough IRQ ids
- *(axvm)* use kspin for IOAPIC forwarding locks
- *(axvm)* mask forwarded IOAPIC host lines
- *(irq)* avoid hard irq controller locks

### Other

- *(axvm)* redesign guest address layout planning ([#1454](https://github.com/rcore-os/tgoskits/pull/1454))
- *(irq-framework)* require boxed IRQ callbacks ([#1452](https://github.com/rcore-os/tgoskits/pull/1452))
- *(axvm)* redesign VM lifecycle state machine ([#1447](https://github.com/rcore-os/tgoskits/pull/1447))
- *(somehal)* modernize x86 qemu irq routing ([#1430](https://github.com/rcore-os/tgoskits/pull/1430))
- *(axvm)* route host IRQs with domain metadata

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.16...axvm-v0.5.17) - 2026-06-27

### Other

- *(platform)* remove ax-config from dynamic runtime path ([#1387](https://github.com/rcore-os/tgoskits/pull/1387))
- *(axdevice)* unify Device model with indexed dispatch and conflict detect ([#1335](https://github.com/rcore-os/tgoskits/pull/1335))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.15...axvm-v0.5.16) - 2026-06-23

### Other

- Enhance archive extraction logic and add legacy file tests ([#1355](https://github.com/rcore-os/tgoskits/pull/1355))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.14...axvm-v0.5.15) - 2026-06-22

### Other

- *(axvm)* route RISC-V IRQs through vPLIC backend ([#1317](https://github.com/rcore-os/tgoskits/pull/1317))
- *(axvm)* add VM interrupt fabric ([#1273](https://github.com/rcore-os/tgoskits/pull/1273))
- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))
- Issue 595 device foundation ([#1258](https://github.com/rcore-os/tgoskits/pull/1258))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.13...axvm-v0.5.14) - 2026-06-12

### Fixed

- *(ci)* stabilize x86 Starry QEMU timing ([#1245](https://github.com/rcore-os/tgoskits/pull/1245))

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.12...axvm-v0.5.13) - 2026-06-11

### Fixed

- fix typos in code and comments across the codebase ([#1206](https://github.com/rcore-os/tgoskits/pull/1206))

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.11...axvm-v0.5.12) - 2026-06-09

### Added

- *(axvisor)* support dynamic x86_64 QEMU guest boot ([#1166](https://github.com/rcore-os/tgoskits/pull/1166))

### Fixed

- *(axvisor)* cache x86 emulated devices directly and harden vCPU interrupt queuing ([#1137](https://github.com/rcore-os/tgoskits/pull/1137))

### Fixed

- publish the corrected feature metadata for host filesystem and platform-dynamic support

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.9...axvm-v0.5.10) - 2026-06-03

### Added

- *(axvisor)* support x86_64 Linux guest boot (vmx) ([#930](https://github.com/rcore-os/tgoskits/pull/930))

### Other

- [AxVisor] add x86_64 UEFI guest support ([#760](https://github.com/rcore-os/tgoskits/pull/760))
- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.8...axvm-v0.5.9) - 2026-05-22

### Other

- updated the following local packages: ax-errno, riscv_vcpu, ax-page-table-multiarch, axaddrspace, axvmconfig, axdevice_base, axvcpu, arm_vcpu, arm_vgic, axdevice, loongarch_vcpu, x86_vcpu

## [0.5.8](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.7...axvm-v0.5.8) - 2026-05-19

### Other

- updated the following local packages: ax-errno, riscv_vcpu, ax-page-table-multiarch, axaddrspace, axvmconfig, axdevice_base, axvcpu, arm_vcpu, arm_vgic, axdevice, loongarch_vcpu, x86_vcpu

## [0.5.7](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.6...axvm-v0.5.7) - 2026-05-15

### Added

- *(axvisor)* support x86_64(VMX) QEMU guest boot ([#526](https://github.com/rcore-os/tgoskits/pull/526))
- *(axvisor)* Add x86_64 AMD SVM support ([#445](https://github.com/rcore-os/tgoskits/pull/445))

### Other

- *(axvm)* inherit workspace dependencies

## [0.5.6](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.5...axvm-v0.5.6) - 2026-04-27

### Added

- *(axvisor)* add loongarch64 qemu support and CI ([#242](https://github.com/rcore-os/tgoskits/pull/242))

### Other

- *(axvisor)* add Linux guest support to the AxVisor riscv64 QEMU test ([#351](https://github.com/rcore-os/tgoskits/pull/351))
