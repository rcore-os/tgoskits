# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.9...x86_vcpu-v0.5.10) - 2026-06-03

### Added

- Enhance SVM support and improve PIT handling for Linux guests ([#1005](https://github.com/rcore-os/tgoskits/pull/1005))
- *(axvisor)* support x86_64 Linux guest boot (vmx) ([#930](https://github.com/rcore-os/tgoskits/pull/930))

### Other

- [AxVisor] add x86_64 UEFI guest support ([#760](https://github.com/rcore-os/tgoskits/pull/760))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.8...x86_vcpu-v0.5.9) - 2026-05-22

### Other

- updated the following local packages: ax-errno, axaddrspace, axdevice_base, axvisor_api, axvcpu, x86_vlapic

## [0.5.8](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.7...x86_vcpu-v0.5.8) - 2026-05-19

### Other

- updated the following local packages: ax-errno, axaddrspace, axdevice_base, axvisor_api, axvcpu, x86_vlapic

## [0.5.7](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.6...x86_vcpu-v0.5.7) - 2026-05-15

### Added

- *(axvisor)* support x86_64(VMX) QEMU guest boot ([#526](https://github.com/rcore-os/tgoskits/pull/526))
- *(axvisor)* Add x86_64 AMD SVM support ([#445](https://github.com/rcore-os/tgoskits/pull/445))

### Other

- *(x86-vcpu)* inherit workspace metadata

## [0.5.6](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.5...x86_vcpu-v0.5.6) - 2026-04-27

### Added

- *(axvisor)* add loongarch64 qemu support and CI ([#242](https://github.com/rcore-os/tgoskits/pull/242))
