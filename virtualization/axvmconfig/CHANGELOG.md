# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.2](https://github.com/rcore-os/tgoskits/compare/axvmconfig-v0.8.1...axvmconfig-v0.8.2) - 2026-07-23

### Other

- *(axvmconfig)* introduce configuration errors ([#1597](https://github.com/rcore-os/tgoskits/pull/1597))

## [0.8.1](https://github.com/rcore-os/tgoskits/compare/axvmconfig-v0.8.0...axvmconfig-v0.8.1) - 2026-07-07

### Other

- updated the following local packages: axvm-types

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/axvmconfig-v0.7.2...axvmconfig-v0.8.0) - 2026-07-02

### Added

- *(axvisor)* support LoongArch Linux guest on QEMU ([#1207](https://github.com/rcore-os/tgoskits/pull/1207))

### Other

- *(axvm)* decouple axvisor arch logic ([#1471](https://github.com/rcore-os/tgoskits/pull/1471))
- *(axvm)* redesign guest address layout planning ([#1454](https://github.com/rcore-os/tgoskits/pull/1454))
- *(axvm)* route host IRQs with domain metadata

## [0.7.2](https://github.com/rcore-os/tgoskits/compare/axvmconfig-v0.7.1...axvmconfig-v0.7.2) - 2026-06-22

### Other

- update Cargo.lock dependencies

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/axvmconfig-v0.7.0...axvmconfig-v0.7.1) - 2026-06-12

### Other

- update Cargo.lock dependencies

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/axvmconfig-v0.6.0...axvmconfig-v0.7.0) - 2026-06-09

### Other

- Refactor Axvisor to unify ArceOS API and improve modularity ([#1019](https://github.com/rcore-os/tgoskits/pull/1019))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/axvmconfig-v0.5.2...axvmconfig-v0.6.0) - 2026-06-03

### Added

- *(axvisor)* support x86_64 Linux guest boot (vmx) ([#930](https://github.com/rcore-os/tgoskits/pull/930))

### Other

- [AxVisor] add x86_64 UEFI guest support ([#760](https://github.com/rcore-os/tgoskits/pull/760))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.5.2](https://github.com/rcore-os/tgoskits/compare/axvmconfig-v0.5.1...axvmconfig-v0.5.2) - 2026-05-22

### Other

- updated the following local packages: ax-errno

## [0.5.1](https://github.com/rcore-os/tgoskits/compare/axvmconfig-v0.5.0...axvmconfig-v0.5.1) - 2026-05-19

### Other

- updated the following local packages: ax-errno

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/axvmconfig-v0.4.8...axvmconfig-v0.5.0) - 2026-05-15

### Added

- *(axvisor)* support x86_64(VMX) QEMU guest boot ([#526](https://github.com/rcore-os/tgoskits/pull/526))

### Other

- *(axvmconfig)* inherit workspace metadata

## [0.4.8](https://github.com/rcore-os/tgoskits/compare/axvmconfig-v0.4.7...axvmconfig-v0.4.8) - 2026-04-27

### Other

- *(axvmconfig)* gate host std support
