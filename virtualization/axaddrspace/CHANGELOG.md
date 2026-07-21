# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/axaddrspace-v0.5.16...axaddrspace-v0.5.17) - 2026-07-21

### Other

- *(ci)* update Rust nightly to 2026-07-15 ([#1626](https://github.com/rcore-os/tgoskits/pull/1626))
- *(axaddrspace)* introduce typed errors ([#1592](https://github.com/rcore-os/tgoskits/pull/1592))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/axaddrspace-v0.5.15...axaddrspace-v0.5.16) - 2026-07-07

### Other

- *(axvm)* use generic nested page tables ([#1477](https://github.com/rcore-os/tgoskits/pull/1477))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/axaddrspace-v0.5.14...axaddrspace-v0.5.15) - 2026-07-02

### Other

- *(axvm)* redesign guest address layout planning ([#1454](https://github.com/rcore-os/tgoskits/pull/1454))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/axaddrspace-v0.5.13...axaddrspace-v0.5.14) - 2026-06-22

### Other

- updated the following local packages: axvm-types

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/axaddrspace-v0.5.12...axaddrspace-v0.5.13) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))

### Other

- Refactor Axvisor to unify ArceOS API and improve modularity ([#1019](https://github.com/rcore-os/tgoskits/pull/1019))

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/axaddrspace-v0.5.11...axaddrspace-v0.5.12) - 2026-06-03

### Other

- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.5.11](https://github.com/rcore-os/tgoskits/compare/axaddrspace-v0.5.10...axaddrspace-v0.5.11) - 2026-05-22

### Other

- updated the following local packages: ax-errno, ax-memory-set, ax-page-table-multiarch

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/axaddrspace-v0.5.9...axaddrspace-v0.5.10) - 2026-05-19

### Other

- updated the following local packages: ax-errno, ax-memory-set, ax-page-table-multiarch

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/axaddrspace-v0.5.8...axaddrspace-v0.5.9) - 2026-05-18

### Fixed

- *(ci)* address usb release and axaddrspace std failures ([#743](https://github.com/rcore-os/tgoskits/pull/743))

## [0.5.8](https://github.com/rcore-os/tgoskits/compare/axaddrspace-v0.5.7...axaddrspace-v0.5.8) - 2026-05-15

### Added

- *(axvisor)* Add x86_64 AMD SVM support ([#445](https://github.com/rcore-os/tgoskits/pull/445))
- *(mm)* track backend split metadata and generate real /proc maps output ([#306](https://github.com/rcore-os/tgoskits/pull/306))

### Other

- *(axaddrspace)* inherit workspace dependencies

## [0.5.7](https://github.com/rcore-os/tgoskits/compare/axaddrspace-v0.5.6...axaddrspace-v0.5.7) - 2026-04-27

### Added

- *(axvisor)* add loongarch64 qemu support and CI ([#242](https://github.com/rcore-os/tgoskits/pull/242))
## 0.1.2

- Add accessor module for memory access.

## 0.1.1

- Support the former four-level EPT build option. By default, level 3 EPT is used. After enabling this option, level 4 EPT is used.

## 0.1.0

- Initial release.
