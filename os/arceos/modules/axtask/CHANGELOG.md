# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.11...ax-task-v0.5.12) - 2026-04-27

### Added

- *(ax-sync)* add mutex lockdep and fix Starry atomic-context violations ([#271](https://github.com/rcore-os/tgoskits/pull/271))

### Fixed

- *(axtask)* register interrupt waker before flag swap ([#316](https://github.com/rcore-os/tgoskits/pull/316))
