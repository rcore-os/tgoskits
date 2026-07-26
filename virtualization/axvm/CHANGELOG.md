# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.24](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.23...axvm-v0.5.24) - 2026-07-23

### Added

- *(axvm)* add virtual interrupt model types and per-vCPU dispatch queue ([#1661](https://github.com/rcore-os/tgoskits/pull/1661))
- *(axdevice)* register exclusive IRQ line resources ([#1630](https://github.com/rcore-os/tgoskits/pull/1630))
- *(axvisor)* Enhance AxLoader and Asus NUC15CRH support with fixes ([#1555](https://github.com/rcore-os/tgoskits/pull/1555))

### Other

- *(cpu-local)* extract per-CPU register ownership ([#1662](https://github.com/rcore-os/tgoskits/pull/1662))
- *(x86_vcpu)* select VMX/SVM backend at runtime from CPUID, rem… ([#1629](https://github.com/rcore-os/tgoskits/pull/1629))
- *(ci)* update Rust nightly to 2026-07-15 ([#1626](https://github.com/rcore-os/tgoskits/pull/1626))
- *(axhvc)* introduce hypercall errors ([#1599](https://github.com/rcore-os/tgoskits/pull/1599))
- *(axvmconfig)* introduce configuration errors ([#1597](https://github.com/rcore-os/tgoskits/pull/1597))
- *(axdevice)* replace errno contracts ([#1595](https://github.com/rcore-os/tgoskits/pull/1595))
- *(axaddrspace)* introduce typed errors ([#1592](https://github.com/rcore-os/tgoskits/pull/1592))
- *(axvm-types)* introduce backend errors ([#1591](https://github.com/rcore-os/tgoskits/pull/1591))
- *(axvm)* introduce typed domain errors ([#1590](https://github.com/rcore-os/tgoskits/pull/1590))
- *(axvm)* consolidate architecture-specific code ([#1562](https://github.com/rcore-os/tgoskits/pull/1562))

## [0.5.23](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.22...axvm-v0.5.23) - 2026-07-10

### Other

- *(riscv_vcpu)* decouple AxVM adapter and clean exits ([#1556](https://github.com/rcore-os/tgoskits/pull/1556))
- *(x86_vcpu)* make x86 virtualization OS-neutral ([#1550](https://github.com/rcore-os/tgoskits/pull/1550))
- *(loongarch_vcpu)* decouple AxVM adapter and typed registers ([#1553](https://github.com/rcore-os/tgoskits/pull/1553))

## [0.5.22](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.21...axvm-v0.5.22) - 2026-07-08

### Other

- updated the following local packages: ax-plat, ax-hal, ax-driver, ax-std

## [0.5.21](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.20...axvm-v0.5.21) - 2026-07-08

### Other

- updated the following local packages: ax-plat, ax-driver, ax-hal, ax-std

## [0.5.20](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.19...axvm-v0.5.20) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, rdrive, ax-plat, ax-driver, arm_vgic, riscv_vplic, x86_vlapic, axdevice, ax-hal, ax-std, loongarch_vcpu, x86_vcpu

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.18...axvm-v0.5.19) - 2026-07-07

### Fixed

- *(ci)* restore Starry ptrace and Axvisor RISC-V tests ([#1521](https://github.com/rcore-os/tgoskits/pull/1521))
- *(block)* drive virtio-blk completions by IRQ ([#1512](https://github.com/rcore-os/tgoskits/pull/1512))

### Other

- *(axvm)* handle vCPU exits in arch adapters ([#1528](https://github.com/rcore-os/tgoskits/pull/1528))
- *(arm_vcpu)* decouple host interface ([#1523](https://github.com/rcore-os/tgoskits/pull/1523))
- *(axvm)* use generic nested page tables ([#1477](https://github.com/rcore-os/tgoskits/pull/1477))
- *(axvm)* migrate fdt handling to fdt-edit ([#1476](https://github.com/rcore-os/tgoskits/pull/1476))
- remove static platform and axconfig generation, make dynamic platform the only path ([#1478](https://github.com/rcore-os/tgoskits/pull/1478))

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/axvm-v0.5.17...axvm-v0.5.18) - 2026-07-02

### Added

- *(axvisor)* support LoongArch Linux guest on QEMU ([#1207](https://github.com/rcore-os/tgoskits/pull/1207))

### Fixed

- *(axvisor)* gate x86 host fs passthrough prepare
- *(axvm)* resolve LoongArch passthrough IRQ ids
- *(axvm)* use kspin for IOAPIC forwarding locks
- *(axvm)* mask forwarded IOAPIC host lines
- *(irq)* avoid hard irq controller locks

### Other

- *(axvm)* decouple axvisor arch logic ([#1471](https://github.com/rcore-os/tgoskits/pull/1471))
- *(axvm)* decouple vcpu backends ([#1467](https://github.com/rcore-os/tgoskits/pull/1467))
- *(axvm)* move VM boot and memory preparation into axvm ([#1462](https://github.com/rcore-os/tgoskits/pull/1462))
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
