# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.6](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.6.5...ax-task-v0.6.6) - 2026-07-24

### Other

- updated the following local packages: ax-hal, ax-hal, ax-ipi, ax-mm

## [0.6.5](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.6.4...ax-task-v0.6.5) - 2026-07-23

### Other

- *(ax-runtime)* centralize UART scheduling ([#1675](https://github.com/rcore-os/tgoskits/pull/1675))
- *(cpu-local)* extract per-CPU register ownership ([#1662](https://github.com/rcore-os/tgoskits/pull/1662))

### Changed

- *(host-test)* initialize and bind CPU zero's dynamic CPU-local area before scheduler startup.

## [0.6.4](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.6.3...ax-task-v0.6.4) - 2026-07-10

### Other

- updated the following local packages: ax-hal, ax-hal, ax-alloc, ax-ipi, ax-mm

## [0.6.3](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.6.2...ax-task-v0.6.3) - 2026-07-08

### Other

- updated the following local packages: ax-hal, ax-hal, ax-alloc, ax-ipi, ax-mm

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.6.1...ax-task-v0.6.2) - 2026-07-08

### Other

- updated the following local packages: ax-alloc, ax-hal, ax-hal, ax-ipi, ax-mm

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.6.0...ax-task-v0.6.1) - 2026-07-08

### Fixed

- *(axtask)* defer cross-core wake instead of spinning on remote on_cpu ([#1495](https://github.com/rcore-os/tgoskits/pull/1495))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.24...ax-task-v0.6.0) - 2026-07-07

### Fixed

- *(ax-task)* avoid RR front insertion on wakeup ([#1532](https://github.com/rcore-os/tgoskits/pull/1532))

### Other

- Remove `ax-feat` crate and redistribute features across runtime, API, and user library layers ([#1513](https://github.com/rcore-os/tgoskits/pull/1513))
- Dev might sleep enhance ([#1480](https://github.com/rcore-os/tgoskits/pull/1480))
- remove static platform and axconfig generation, make dynamic platform the only path ([#1478](https://github.com/rcore-os/tgoskits/pull/1478))

## [0.5.24](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.23...ax-task-v0.5.24) - 2026-07-02

### Added

- *(irq-framework)* use domain-scoped irq ids
- *(kspin)* add lockdep-aware spin rwlock ([#1397](https://github.com/rcore-os/tgoskits/pull/1397))

### Fixed

- *(ci)* prevent Starry qemu hangs in IRQ paths ([#1431](https://github.com/rcore-os/tgoskits/pull/1431))
- *(ax-task)* harden SMP wake and migration rescheduling ([#1426](https://github.com/rcore-os/tgoskits/pull/1426))
- *(irq)* separate IRQ domains from trap vectors ([#1346](https://github.com/rcore-os/tgoskits/pull/1346))

### Other

- *(irq-framework)* require boxed IRQ callbacks ([#1452](https://github.com/rcore-os/tgoskits/pull/1452))
- *(platforms)* remove LoongArch static platform ([#1428](https://github.com/rcore-os/tgoskits/pull/1428))
- *(build)* generate build.rs Rust sources with quote ([#1422](https://github.com/rcore-os/tgoskits/pull/1422))
- Revert "fix(irq): separate IRQ domains from trap vectors ([#1346](https://github.com/rcore-os/tgoskits/pull/1346))" ([#1424](https://github.com/rcore-os/tgoskits/pull/1424))

## [0.5.23](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.22...ax-task-v0.5.23) - 2026-06-27

### Fixed

- *(ax-task)* clear delivered remote reschedule requests ([#1381](https://github.com/rcore-os/tgoskits/pull/1381))

### Other

- *(platform)* remove ax-config from dynamic runtime path ([#1387](https://github.com/rcore-os/tgoskits/pull/1387))
- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.5.22](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.21...ax-task-v0.5.22) - 2026-06-23

### Other

- updated the following local packages: ax-hal, ax-hal, ax-lockdep, ax-kspin, ax-alloc, axpoll, ax-ipi, ax-mm

## [0.5.21](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.20...ax-task-v0.5.21) - 2026-06-22

### Added

- *(poll)* add irq-safe deferred notifications ([#1278](https://github.com/rcore-os/tgoskits/pull/1278))

### Fixed

- *(ax-task)* prioritize ready poll_io before interrupt ([#1337](https://github.com/rcore-os/tgoskits/pull/1337))

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.5.20](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.19...ax-task-v0.5.20) - 2026-06-12

### Fixed

- *(starry)* reprogram timer for short deadlines ([#1250](https://github.com/rcore-os/tgoskits/pull/1250))
- *(axtask)* improve might_sleep diagnostics and coverage ([#1235](https://github.com/rcore-os/tgoskits/pull/1235))
- *(axtask)* use monotonic deadlines for sleeps ([#1240](https://github.com/rcore-os/tgoskits/pull/1240))
- *(axruntime)* ensure aarch64 SMP IPI readiness before app init ([#1196](https://github.com/rcore-os/tgoskits/pull/1196))

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.18...ax-task-v0.5.19) - 2026-06-11

### Fixed

- fix typos in code and comments across the codebase ([#1206](https://github.com/rcore-os/tgoskits/pull/1206))

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.17...ax-task-v0.5.18) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))
- *(starry-kernel)* eBPF kernel runtime (tracepoint / kprobe / perf) ([#886](https://github.com/rcore-os/tgoskits/pull/886))

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/ax-task-v0.5.16...ax-task-v0.5.17) - 2026-06-03

### Added

- *(axtask)* prefer current CPU in select_run_queue for cache affinity ([#1012](https://github.com/rcore-os/tgoskits/pull/1012))
- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(axtask)* replace PREV_TASK Weak<AxTask> with raw pointer ([#996](https://github.com/rcore-os/tgoskits/pull/996))
- *(axtask)* add task stack guard page support ([#811](https://github.com/rcore-os/tgoskits/pull/811))

### Fixed

- *(axtask)* kick remote CPUs on SMP wakeups ([#926](https://github.com/rcore-os/tgoskits/pull/926))
- *(ax-task)* preempt on async wake, guard wait queue against double-enqueue ([#912](https://github.com/rcore-os/tgoskits/pull/912))
- *(signal)* add wake_task after signal delivery and dumpable/no_new_privs fields ([#797](https://github.com/rcore-os/tgoskits/pull/797))

### Other

- *(sched)* add sched-family test suite and fix kernel scheduler sys… ([#986](https://github.com/rcore-os/tgoskits/pull/986))
- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- Revert "fix(ax-task): preempt on async wake, guard wait queue against double-…" ([#939](https://github.com/rcore-os/tgoskits/pull/939))

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
