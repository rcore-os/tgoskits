# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.21](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.20...arm_vcpu-v0.5.21) - 2026-08-27

### Fixed

- *(arm_vcpu)* synchronize EL2 enable publication ([#2191](https://github.com/rcore-os/tgoskits/pull/2191))

## [0.5.20](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.19...arm_vcpu-v0.5.20) - 2026-08-20

### Added

- *(axvisor)* Implement inter-VM communication (IVC) demo and protocol enhancements ([#1834](https://github.com/rcore-os/tgoskits/pull/1834))

### Fixed

- *(arm-vcpu)* preserve HVC exception PC ([#1953](https://github.com/rcore-os/tgoskits/pull/1953))
- *(aarch64)* restore RK3588 guest cpufreq and SCMI support ([#1919](https://github.com/rcore-os/tgoskits/pull/1919))

### Other

- *(sync)* unify lock primitives in ax-sync ([#1956](https://github.com/rcore-os/tgoskits/pull/1956))

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.18...arm_vcpu-v0.5.19) - 2026-08-09

### Other

- *(axvisor)* remove NimbOS guest, legacy CI, and standalone scripts ([#1866](https://github.com/rcore-os/tgoskits/pull/1866))
- *(axvm)* unify guest devices and AArch64 timer ownership ([#1717](https://github.com/rcore-os/tgoskits/pull/1717))

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.17...arm_vcpu-v0.5.18) - 2026-08-03

### Fixed

- *(axhvc)* handle PSCI_VERSION hypercall ([#1692](https://github.com/rcore-os/tgoskits/pull/1692))

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.16...arm_vcpu-v0.5.17) - 2026-07-23

### Other

- *(cpu-local)* extract per-CPU register ownership ([#1662](https://github.com/rcore-os/tgoskits/pull/1662))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.15...arm_vcpu-v0.5.16) - 2026-07-07

### Other

- *(arm_vcpu)* decouple host interface ([#1523](https://github.com/rcore-os/tgoskits/pull/1523))
- *(axvm)* use generic nested page tables ([#1477](https://github.com/rcore-os/tgoskits/pull/1477))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.14...arm_vcpu-v0.5.15) - 2026-07-02

### Other

- *(axvm)* decouple axvisor arch logic ([#1471](https://github.com/rcore-os/tgoskits/pull/1471))
- *(axvm)* decouple vcpu backends ([#1467](https://github.com/rcore-os/tgoskits/pull/1467))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.13...arm_vcpu-v0.5.14) - 2026-06-27

### Other

- updated the following local packages: axvcpu

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.12...arm_vcpu-v0.5.13) - 2026-06-23

### Other

- updated the following local packages: axvcpu

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.11...arm_vcpu-v0.5.12) - 2026-06-22

### Other

- updated the following local packages: axvcpu

## [0.5.11](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.10...arm_vcpu-v0.5.11) - 2026-06-09

### Other

- Refactor Axvisor to unify ArceOS API and improve modularity ([#1019](https://github.com/rcore-os/tgoskits/pull/1019))

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.9...arm_vcpu-v0.5.10) - 2026-06-03

### Other

- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.8...arm_vcpu-v0.5.9) - 2026-05-22

### Other

- updated the following local packages: ax-errno, axaddrspace, axdevice_base, axvcpu

## [0.5.8](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.7...arm_vcpu-v0.5.8) - 2026-05-19

### Other

- updated the following local packages: ax-errno, axaddrspace, axdevice_base, axvcpu

## [0.5.7](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.6...arm_vcpu-v0.5.7) - 2026-05-18

### Other

- updated the following local packages: axaddrspace, axdevice_base, axvcpu

## [0.5.6](https://github.com/rcore-os/tgoskits/compare/arm_vcpu-v0.5.5...arm_vcpu-v0.5.6) - 2026-05-15

### Other

- *(arm-vcpu)* inherit workspace metadata
- *(repo)* split non-USB clippy cleanups ([#372](https://github.com/rcore-os/tgoskits/pull/372))
## 0.1.1

- Support the former four-level EPT build option. By default, level 3 EPT is used. After enabling this option, level 4 EPT is used.

## 0.1.0

- Initial release.
