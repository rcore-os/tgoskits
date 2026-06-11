# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
