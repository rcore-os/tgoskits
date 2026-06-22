# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
