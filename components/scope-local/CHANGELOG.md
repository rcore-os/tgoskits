# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Require a CPU pin for active per-CPU scope lookup and replace
  `LocalItem::Deref` with non-escaping closure access. `clone_current` keeps
  pins short for owned values, while `try_with_pinned` provides a
  non-allocating IRQ path after task-context initialization.
- Add construction-only `ScopeCell` item initialization that requires an
  exclusive, unpublished cell and never enters a preemption context. This lets
  runtimes prepare allocating process resources before scheduler publication.

## [0.4.2](https://github.com/rcore-os/tgoskits/compare/scope-local-v0.4.1...scope-local-v0.4.2) - 2026-07-07

### Other

- *(repo)* remove vendored spin crate ([#1421](https://github.com/rcore-os/tgoskits/pull/1421))

## [0.4.1](https://github.com/rcore-os/tgoskits/compare/scope-local-v0.4.0...scope-local-v0.4.1) - 2026-06-27

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))

## [0.4.0](https://github.com/rcore-os/tgoskits/compare/scope-local-v0.3.8...scope-local-v0.4.0) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))

## [0.3.8](https://github.com/rcore-os/tgoskits/compare/scope-local-v0.3.7...scope-local-v0.3.8) - 2026-06-03

### Other

- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))

## [0.3.7](https://github.com/rcore-os/tgoskits/compare/scope-local-v0.3.6...scope-local-v0.3.7) - 2026-05-15

### Other

- *(scope-local)* inherit workspace metadata
