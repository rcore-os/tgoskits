# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.18](https://github.com/rcore-os/tgoskits/compare/x86_vlapic-v0.4.17...x86_vlapic-v0.4.18) - 2026-07-07

### Other

- updated the following local packages: ax-kspin, axvm-types, axdevice_base

## [0.4.17](https://github.com/rcore-os/tgoskits/compare/x86_vlapic-v0.4.16...x86_vlapic-v0.4.17) - 2026-07-02

### Other

- *(axvm)* route host IRQs with domain metadata

## [0.4.16](https://github.com/rcore-os/tgoskits/compare/x86_vlapic-v0.4.15...x86_vlapic-v0.4.16) - 2026-06-27

### Other

- updated the following local packages: axdevice_base

## [0.4.15](https://github.com/rcore-os/tgoskits/compare/x86_vlapic-v0.4.14...x86_vlapic-v0.4.15) - 2026-06-23

### Other

- updated the following local packages: ax-kspin

## [0.4.14](https://github.com/rcore-os/tgoskits/compare/x86_vlapic-v0.4.13...x86_vlapic-v0.4.14) - 2026-06-22

### Other

- updated the following local packages: axvm-types, axdevice_base

## [0.4.13](https://github.com/rcore-os/tgoskits/compare/x86_vlapic-v0.4.12...x86_vlapic-v0.4.13) - 2026-06-09

### Added

- *(axvisor)* support dynamic x86_64 QEMU guest boot ([#1166](https://github.com/rcore-os/tgoskits/pull/1166))

### Fixed

- *(axvisor)* cache x86 emulated devices directly and harden vCPU interrupt queuing ([#1137](https://github.com/rcore-os/tgoskits/pull/1137))

### Fixed

- publish the host interface module used by `axvm`

## [0.4.11](https://github.com/rcore-os/tgoskits/compare/x86_vlapic-v0.4.10...x86_vlapic-v0.4.11) - 2026-06-03

### Added

- Enhance SVM support and improve PIT handling for Linux guests ([#1005](https://github.com/rcore-os/tgoskits/pull/1005))
- *(axvisor)* support x86_64 Linux guest boot (vmx) ([#930](https://github.com/rcore-os/tgoskits/pull/930))

### Other

- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.4.10](https://github.com/rcore-os/tgoskits/compare/x86_vlapic-v0.4.9...x86_vlapic-v0.4.10) - 2026-05-22

### Other

- updated the following local packages: ax-errno, axaddrspace, axdevice_base

## [0.4.9](https://github.com/rcore-os/tgoskits/compare/x86_vlapic-v0.4.8...x86_vlapic-v0.4.9) - 2026-05-19

### Other

- updated the following local packages: ax-errno, axaddrspace, axdevice_base

## [0.4.8](https://github.com/rcore-os/tgoskits/compare/x86_vlapic-v0.4.7...x86_vlapic-v0.4.8) - 2026-05-15

### Other

- *(x86-vlapic)* inherit workspace metadata
