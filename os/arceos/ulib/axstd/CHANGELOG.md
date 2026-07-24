# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.30](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.29...ax-std-v0.5.30) - 2026-07-24

### Other

- updated the following local packages: ax-driver, ax-runtime, ax-hal, ax-task, ax-api, ax-posix-api

## [0.5.29](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.28...ax-std-v0.5.29) - 2026-07-23

### Other

- *(axbuild)* 将构建与启动能力收敛到显式配置 ([#1620](https://github.com/rcore-os/tgoskits/pull/1620))

## [0.5.28](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.27...ax-std-v0.5.28) - 2026-07-10

### Other

- updated the following local packages: ax-driver, ax-hal, ax-runtime, ax-alloc, ax-task, ax-api, ax-posix-api

## [0.5.27](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.26...ax-std-v0.5.27) - 2026-07-08

### Other

- updated the following local packages: ax-hal, ax-runtime, ax-alloc, ax-driver, ax-task, ax-api, ax-posix-api

## [0.5.26](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.25...ax-std-v0.5.26) - 2026-07-08

### Other

- updated the following local packages: ax-alloc, ax-driver, ax-hal, ax-task, ax-runtime, ax-api, ax-posix-api

## [0.5.25](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.24...ax-std-v0.5.25) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, ax-task, ax-alloc, ax-driver, ax-hal, ax-runtime, ax-api, ax-posix-api

## [0.5.24](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.23...ax-std-v0.5.24) - 2026-07-07

### Other

- Remove `ax-feat` crate and redistribute features across runtime, API, and user library layers ([#1513](https://github.com/rcore-os/tgoskits/pull/1513))
- *(platforms)* move someboot and somehal-macros and add documents ([#1485](https://github.com/rcore-os/tgoskits/pull/1485))
- remove static platform and axconfig generation, make dynamic platform the only path ([#1478](https://github.com/rcore-os/tgoskits/pull/1478))

## [0.5.23](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.22...ax-std-v0.5.23) - 2026-07-02

### Other

- *(ax-driver)* remove static platform compatibility ([#1463](https://github.com/rcore-os/tgoskits/pull/1463))
- *(platforms)* remove LoongArch static platform ([#1428](https://github.com/rcore-os/tgoskits/pull/1428))
- *(ax-runtime)* resolve device IRQ bindings to IrqId

## [0.5.22](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.21...ax-std-v0.5.22) - 2026-06-27

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))

## [0.5.21](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.20...ax-std-v0.5.21) - 2026-06-23

### Other

- updated the following local packages: ax-hal, ax-runtime, ax-api, ax-posix-api, ax-kspin, ax-alloc, ax-driver, ax-feat

## [0.5.20](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.19...ax-std-v0.5.20) - 2026-06-22

### Added

- *(axruntime)* add compiler-backed stack protector support ([#1239](https://github.com/rcore-os/tgoskits/pull/1239))

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.18...ax-std-v0.5.19) - 2026-06-12

### Fixed

- *(axtask)* improve might_sleep diagnostics and coverage ([#1235](https://github.com/rcore-os/tgoskits/pull/1235))
- *(axtask)* use monotonic deadlines for sleeps ([#1240](https://github.com/rcore-os/tgoskits/pull/1240))

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.17...ax-std-v0.5.18) - 2026-06-11

### Other

- *(axvisor)* remove obsolete x86 q35 static platform ([#1186](https://github.com/rcore-os/tgoskits/pull/1186))

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.16...ax-std-v0.5.17) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.15...ax-std-v0.5.16) - 2026-06-03

### Added

- *(axtask)* add task stack guard page support ([#811](https://github.com/rcore-os/tgoskits/pull/811))

### Fixed

- *(axvisor)* enable buddy-slab allocator ([#974](https://github.com/rcore-os/tgoskits/pull/974))

### Other

- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.14...ax-std-v0.5.15) - 2026-05-22

### Other

- updated the following local packages: ax-errno, ax-feat, ax-io, ax-api

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.13...ax-std-v0.5.14) - 2026-05-19

### Other

- updated the following local packages: ax-errno, ax-io, ax-feat, ax-api

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/ax-std-v0.5.12...ax-std-v0.5.13) - 2026-05-15

### Other

- updated the following local packages: ax-kspin, ax-io, ax-feat, ax-api
