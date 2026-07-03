# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

- Support the new 4-level-ept feature. By default, level 3 ept is used. After enabling this feature, level 4 ept is used.

## 0.1.0

- Initial release.