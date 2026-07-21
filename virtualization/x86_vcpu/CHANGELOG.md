# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.20](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.19...x86_vcpu-v0.5.20) - 2026-07-21

### Added

- *(axvisor)* Enhance AxLoader and Asus NUC15CRH support with fixes ([#1555](https://github.com/rcore-os/tgoskits/pull/1555))

### Other

- *(x86_vcpu)* select VMX/SVM backend at runtime from CPUID, rem… ([#1629](https://github.com/rcore-os/tgoskits/pull/1629))

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.18...x86_vcpu-v0.5.19) - 2026-07-10

### Other

- *(x86_vcpu)* make x86 virtualization OS-neutral ([#1550](https://github.com/rcore-os/tgoskits/pull/1550))

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.17...x86_vcpu-v0.5.18) - 2026-07-08

### Other

- updated the following local packages: x86_vlapic

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.16...x86_vcpu-v0.5.17) - 2026-07-07

### Other

- *(axvm)* handle vCPU exits in arch adapters ([#1528](https://github.com/rcore-os/tgoskits/pull/1528))
- *(axvm)* use generic nested page tables ([#1477](https://github.com/rcore-os/tgoskits/pull/1477))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.15...x86_vcpu-v0.5.16) - 2026-07-02

### Other

- *(axvm)* decouple axvisor arch logic ([#1471](https://github.com/rcore-os/tgoskits/pull/1471))
- *(axvm)* decouple vcpu backends ([#1467](https://github.com/rcore-os/tgoskits/pull/1467))
- *(axvm)* route host IRQs with domain metadata

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.14...x86_vcpu-v0.5.15) - 2026-06-27

### Other

- updated the following local packages: axdevice_base, axvcpu, x86_vlapic

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.13...x86_vcpu-v0.5.14) - 2026-06-23

### Other

- updated the following local packages: axvcpu, x86_vlapic

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.12...x86_vcpu-v0.5.13) - 2026-06-22

### Other

- updated the following local packages: axvm-types, axdevice_base, axvcpu, x86_vlapic

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.11...x86_vcpu-v0.5.12) - 2026-06-09

### Added

- *(axvisor)* support dynamic x86_64 QEMU guest boot ([#1166](https://github.com/rcore-os/tgoskits/pull/1166))

### Fixed

- publish the host interface module used by `axvm`

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.9...x86_vcpu-v0.5.10) - 2026-06-03

### Added

- Enhance SVM support and improve PIT handling for Linux guests ([#1005](https://github.com/rcore-os/tgoskits/pull/1005))
- *(axvisor)* support x86_64 Linux guest boot (vmx) ([#930](https://github.com/rcore-os/tgoskits/pull/930))

### Other

- [AxVisor] add x86_64 UEFI guest support ([#760](https://github.com/rcore-os/tgoskits/pull/760))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.8...x86_vcpu-v0.5.9) - 2026-05-22

### Other

- updated the following local packages: ax-errno, axaddrspace, axdevice_base, axvcpu, x86_vlapic

## [0.5.8](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.7...x86_vcpu-v0.5.8) - 2026-05-19

### Other

- updated the following local packages: ax-errno, axaddrspace, axdevice_base, axvcpu, x86_vlapic

## [0.5.7](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.6...x86_vcpu-v0.5.7) - 2026-05-15

### Added

- *(axvisor)* support x86_64(VMX) QEMU guest boot ([#526](https://github.com/rcore-os/tgoskits/pull/526))
- *(axvisor)* Add x86_64 AMD SVM support ([#445](https://github.com/rcore-os/tgoskits/pull/445))

### Other

- *(x86-vcpu)* inherit workspace metadata

## [0.5.6](https://github.com/rcore-os/tgoskits/compare/x86_vcpu-v0.5.5...x86_vcpu-v0.5.6) - 2026-04-27

### Added

- *(axvisor)* add loongarch64 qemu support and CI ([#242](https://github.com/rcore-os/tgoskits/pull/242))
