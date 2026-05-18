# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
