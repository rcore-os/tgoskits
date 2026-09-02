# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Expose the typed PCI host key so architecture composition roots can register a provider only when referenced.
- Add architecture-neutral ECAM, memory-aperture, and root lifecycle frontends for graph-resolved PCI hosts.
- Add fixed versus relocatable 32-bit BAR decode policies and preserve prefetchable attributes across probes, partial writes, and reset.
- Model Memory Space Enable, Bus Master Enable, and INTx Disable command state through an out-of-lock effect boundary.

### Fixed

- Reset every bound PCI endpoint after restoring root state, continue after failures, and return the first endpoint error.

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.6.1...axdevice-v0.6.2) - 2026-08-27

### Fixed

- *(virtualization)* port isolated interrupt controller fixes ([#2199](https://github.com/rcore-os/tgoskits/pull/2199))

### Other

- *(axvm)* unify host timer ownership ([#2190](https://github.com/rcore-os/tgoskits/pull/2190))

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.6.0...axdevice-v0.6.1) - 2026-08-25

### Fixed

- *(axdevice)* propagate unrecoverable fw_cfg DMA faults ([#2139](https://github.com/rcore-os/tgoskits/pull/2139))

### Other

- *(axvm)* unify virtual device registration ([#2138](https://github.com/rcore-os/tgoskits/pull/2138))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.5.7...axdevice-v0.6.0) - 2026-08-20

### Added

- *(axvisor)* Implement inter-VM communication (IVC) demo and protocol enhancements ([#1834](https://github.com/rcore-os/tgoskits/pull/1834))
- *(axvisor)* add dual-guest virtio-net support ([#1927](https://github.com/rcore-os/tgoskits/pull/1927))

### Fixed

- *(axdevice)* [**breaking**] bind device access to the issuing vCPU ([#2092](https://github.com/rcore-os/tgoskits/pull/2092))

### Other

- *(axtest)* standardize Cargo and QEMU test flow ([#2088](https://github.com/rcore-os/tgoskits/pull/2088))
- *(sync)* unify lock primitives in ax-sync ([#1956](https://github.com/rcore-os/tgoskits/pull/1956))

## [0.5.7](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.5.6...axdevice-v0.5.7) - 2026-08-09

### Added

- *(axvisor)* build VMs from a resolved device graph ([#1718](https://github.com/rcore-os/tgoskits/pull/1718))

### Fixed

- *(axdevice)* correct fw_cfg DMA fault handling ([#1918](https://github.com/rcore-os/tgoskits/pull/1918))

### Other

- *(axvm)* unify guest devices and AArch64 timer ownership ([#1717](https://github.com/rcore-os/tgoskits/pull/1717))

## [0.5.6](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.5.5...axdevice-v0.5.6) - 2026-08-03

### Added

- *(axvm)* add VmInterruptSender and integrate dispatcher into VmRuntimeHandle ([#1679](https://github.com/rcore-os/tgoskits/pull/1679))

### Fixed

- *(virtualization)* avoid privileged IRQ ops in host tests ([#1776](https://github.com/rcore-os/tgoskits/pull/1776))

### Other

- *(axvisor)* implement unified emulated device framework ([#1722](https://github.com/rcore-os/tgoskits/pull/1722))

## [0.5.5](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.5.4...axdevice-v0.5.5) - 2026-07-23

### Added

- *(axdevice)* register exclusive IRQ line resources ([#1630](https://github.com/rcore-os/tgoskits/pull/1630))

### Other

- *(axdevice)* replace errno contracts ([#1595](https://github.com/rcore-os/tgoskits/pull/1595))

## [0.5.4](https://github.com/rcore-os/tgoskits/compare/axdevice-v0.5.3...axdevice-v0.5.4) - 2026-07-10

### Other

- *(x86_vcpu)* make x86 virtualization OS-neutral ([#1550](https://github.com/rcore-os/tgoskits/pull/1550))

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
