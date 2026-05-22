# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.15...ax-task-v0.5.16) - 2026-05-22

### Fixed

- *(ax-task)* migrate tasks after affinity updates ([#825](https://github.com/rcore-os/tgoskits/pull/825))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.14...ax-task-v0.5.15) - 2026-05-19

### Fixed

- *(starry)* weston bringup fixes + IRQ wakers + AF_UNIX cmsg byte marks ([#509](https://github.com/rcore-os/tgoskits/pull/509))
- *(unix-stream,poll_io)* non-blocking accept, peer EOF, waker registration ([#697](https://github.com/rcore-os/tgoskits/pull/697))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.13...ax-task-v0.5.14) - 2026-05-15

### Other

- updated the following local packages: ax-cpumask, ax-kspin, axpoll, ax-sched, ax-timer-list, ax-config, ax-hal, ax-hal

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.11...ax-task-v0.5.12) - 2026-04-27

### Added

- *(ax-sync)* add mutex lockdep and fix Starry atomic-context violations ([#271](https://github.com/rcore-os/tgoskits/pull/271))

### Fixed

- *(axtask)* register interrupt waker before flag swap ([#316](https://github.com/rcore-os/tgoskits/pull/316))
