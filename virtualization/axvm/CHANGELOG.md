# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
