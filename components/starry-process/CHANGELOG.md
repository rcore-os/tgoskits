# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.8](https://github.com/rcore-os/tgoskits/compare/starry-process-v0.4.7...starry-process-v0.4.8) - 2026-06-03

### Added

- *(starry-kernel)* implement child subreaper ([#1050](https://github.com/rcore-os/tgoskits/pull/1050))
- *(axtask)* add task stack guard page support ([#811](https://github.com/rcore-os/tgoskits/pull/811))

### Added

- Track child subreaper state and reparent orphans to the nearest living subreaper before falling back to init.

## [0.4.7](https://github.com/rcore-os/tgoskits/compare/starry-process-v0.4.6...starry-process-v0.4.7) - 2026-05-22

### Added

- *(starry)* support multi-threaded execve ([#273](https://github.com/rcore-os/tgoskits/pull/273))

## [0.4.6](https://github.com/rcore-os/tgoskits/compare/starry-process-v0.4.5...starry-process-v0.4.6) - 2026-05-15

### Other

- *(starry-process)* inherit workspace metadata
