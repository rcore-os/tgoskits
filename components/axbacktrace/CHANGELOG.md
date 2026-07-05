# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.4](https://github.com/rcore-os/tgoskits/compare/axbacktrace-v0.4.3...axbacktrace-v0.4.4) - 2026-06-27

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))

## [0.4.3](https://github.com/rcore-os/tgoskits/compare/axbacktrace-v0.4.2...axbacktrace-v0.4.3) - 2026-06-23

### Other

- updated the following local packages: axpanic

## [0.4.2](https://github.com/rcore-os/tgoskits/compare/axbacktrace-v0.4.1...axbacktrace-v0.4.2) - 2026-06-09

### Added

- *(backtrace)* add showcase workflow ([#1094](https://github.com/rcore-os/tgoskits/pull/1094))

## [0.4.1](https://github.com/rcore-os/tgoskits/compare/axbacktrace-v0.4.0...axbacktrace-v0.4.1) - 2026-06-03

### Fixed

- *(axbacktrace)* harden correctness, optimize allocation, and add per-arch IP adjustment ([#1029](https://github.com/rcore-os/tgoskits/pull/1029))

### Other

- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))

## [0.4.0](https://github.com/rcore-os/tgoskits/compare/axbacktrace-v0.3.9...axbacktrace-v0.4.0) - 2026-05-22

### Other

- *(axbacktrace)* use Backtrace::kind() instead of BacktraceReport ([#748](https://github.com/rcore-os/tgoskits/pull/748))

## [0.3.9](https://github.com/rcore-os/tgoskits/compare/axbacktrace-v0.3.8...axbacktrace-v0.3.9) - 2026-05-15

### Added

- *(ax-runtime)* add panic recursion guards ([#420](https://github.com/rcore-os/tgoskits/pull/420))

### Other

- *(axbacktrace)* inherit workspace metadata
