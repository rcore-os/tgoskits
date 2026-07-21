# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.2](https://github.com/rcore-os/tgoskits/compare/axpoll-v0.5.1...axpoll-v0.5.2) - 2026-07-21

### Other

- update Cargo.toml dependencies

## [0.5.1](https://github.com/rcore-os/tgoskits/compare/axpoll-v0.5.0...axpoll-v0.5.1) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, ax-kspin

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/axpoll-v0.4.3...axpoll-v0.5.0) - 2026-07-07

### Other

- Remove `ax-feat` crate and redistribute features across runtime, API, and user library layers ([#1513](https://github.com/rcore-os/tgoskits/pull/1513))
- *(repo)* remove vendored spin crate ([#1421](https://github.com/rcore-os/tgoskits/pull/1421))

## [0.4.3](https://github.com/rcore-os/tgoskits/compare/axpoll-v0.4.2...axpoll-v0.4.3) - 2026-07-02

### Other

- updated the following local packages: ax-kspin, ax-kspin

## [0.4.2](https://github.com/rcore-os/tgoskits/compare/axpoll-v0.4.1...axpoll-v0.4.2) - 2026-06-27

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))

## [0.4.1](https://github.com/rcore-os/tgoskits/compare/axpoll-v0.4.0...axpoll-v0.4.1) - 2026-06-23

### Other

- updated the following local packages: ax-kspin, ax-kspin

## [0.4.0](https://github.com/rcore-os/tgoskits/compare/axpoll-v0.3.11...axpoll-v0.4.0) - 2026-06-22

### Added

- *(starry)* add Wayland app case ([#1160](https://github.com/rcore-os/tgoskits/pull/1160))
- *(poll)* add irq-safe deferred notifications ([#1278](https://github.com/rcore-os/tgoskits/pull/1278))

## [0.3.11](https://github.com/rcore-os/tgoskits/compare/axpoll-v0.3.10...axpoll-v0.3.11) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))

## [0.3.10](https://github.com/rcore-os/tgoskits/compare/axpoll-v0.3.9...axpoll-v0.3.10) - 2026-06-03

### Other

- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))

## [0.3.9](https://github.com/rcore-os/tgoskits/compare/axpoll-v0.3.8...axpoll-v0.3.9) - 2026-05-15

### Added

- *(runtime)* extend IRQ, RTC, and tty event support ([#287](https://github.com/rcore-os/tgoskits/pull/287))

### Other

- *(axpoll)* inherit workspace metadata
