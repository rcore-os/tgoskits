# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/loongarch-intc-driver-v0.1.1...loongarch-intc-driver-v0.2.0) - 2026-11-27

### Added

- *(axvisor)* boot Linux with Web management on Loongson 3A6000 ([#2799](https://github.com/rcore-os/tgoskits/pull/2799))

### Fixed

- *(loongarch-intc-driver)* [**breaking**] acknowledge events before dispatch ([#2857](https://github.com/rcore-os/tgoskits/pull/2857))

### Changed

- Acknowledge EIOINTC pending vectors inside `claim()` before device dispatch.
  Remove the separate EIO `complete()` method so handler return cannot clear
  an interrupt raised during service; consumers must remove deferred completion.

## [0.1.1](https://github.com/rcore-os/tgoskits/compare/loongarch-intc-driver-v0.1.0...loongarch-intc-driver-v0.1.1) - 2026-09-09

### Other

- *(ci)* update Rust nightly to 2026-09-04 ([#2273](https://github.com/rcore-os/tgoskits/pull/2273))
