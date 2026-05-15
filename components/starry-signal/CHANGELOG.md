# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/starry-signal-v0.6.0...starry-signal-v0.6.1) - 2026-05-15

### Added

- *(timer)* implement POSIX timer syscalls (timer_create/settime/gettime/delete ([#341](https://github.com/rcore-os/tgoskits/pull/341))

### Fixed

- *(starryos)* restore login shell startup ([#427](https://github.com/rcore-os/tgoskits/pull/427))

### Other

- *(starry-signal)* fix cross-arch restore assumptions and document prior stack-isolation fix ([#468](https://github.com/rcore-os/tgoskits/pull/468))
- *(starry-signal)* inherit workspace metadata
- update ax-cpu and starry-signal dependencies to version 0.6

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/starry-signal-v0.5.7...starry-signal-v0.6.0) - 2026-04-27

### Added

- *(ax-sync)* add mutex lockdep and fix Starry atomic-context violations ([#271](https://github.com/rcore-os/tgoskits/pull/271))
