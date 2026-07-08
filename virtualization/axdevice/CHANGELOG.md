# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.3](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.5.2...axdevice-v0.5.3) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, arm_vgic, riscv_vplic, x86_vlapic

## [0.5.2](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.5.1...axdevice-v0.5.2) - 2026-07-07

### Other

- updated the following local packages: ax-kspin, axvm-types, arm_vgic, axdevice_base, riscv_vplic, x86_vlapic

## [0.5.1](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.5.0...axdevice-v0.5.1) - 2026-07-02

### Added

- *(axvisor)* support LoongArch Linux guest on QEMU ([#1207](https://github.com/rcore-os/tgoskits/pull/1207))

### Other

- *(axvm)* route host IRQs with domain metadata

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.4.14...axdevice-v0.5.0) - 2026-06-27

### Other

- *(axdevice)* unify Device model with indexed dispatch and conflict detect ([#1335](https://github.com/rcore-os/tgoskits/pull/1335))

## [0.4.14](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.4.13...axdevice-v0.4.14) - 2026-06-23

### Other

- updated the following local packages: ax-kspin, arm_vgic, riscv_vplic, x86_vlapic

## [0.4.13](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.4.12...axdevice-v0.4.13) - 2026-06-22

### Other

- Issue 595 device foundation ([#1258](https://github.com/rcore-os/tgoskits/pull/1258))

## [0.4.12](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.4.11...axdevice-v0.4.12) - 2026-06-09

### Fixed

- *(axvisor)* cache x86 emulated devices directly and harden vCPU interrupt queuing ([#1137](https://github.com/rcore-os/tgoskits/pull/1137))

### Other

- Refactor Axvisor to unify ArceOS API and improve modularity ([#1019](https://github.com/rcore-os/tgoskits/pull/1019))

## [0.4.11](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.4.10...axdevice-v0.4.11) - 2026-06-03

### Added

- *(axvisor)* support x86_64 Linux guest boot (vmx) ([#930](https://github.com/rcore-os/tgoskits/pull/930))

### Other

- Remove range-alloc-arceos crate and its associated files ([#991](https://github.com/rcore-os/tgoskits/pull/991))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.4.10](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.4.9...axdevice-v0.4.10) - 2026-05-22

### Other

- updated the following local packages: ax-errno, axaddrspace, axvmconfig, axdevice_base, arm_vgic, riscv_vplic

## [0.4.9](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.4.8...axdevice-v0.4.9) - 2026-05-19

### Other

- updated the following local packages: ax-errno, riscv_vplic, axaddrspace, axvmconfig, axdevice_base, arm_vgic

## [0.4.8](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.4.7...axdevice-v0.4.8) - 2026-05-15

### Other

- *(axdevice)* inherit workspace dependencies
