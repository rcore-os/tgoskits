# Changelog

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/ax-cpu-v0.6.8...ax-cpu-v0.7.0) - 2026-06-22

### Fixed

- *(axcpu)* preserve AVX state across x86_64 context switch (XSAVE) ([#1329](https://github.com/rcore-os/tgoskits/pull/1329))

### Other

- Feat/x86 64 ptrace clean ([#1062](https://github.com/rcore-os/tgoskits/pull/1062))

## [0.6.8](https://github.com/rcore-os/tgoskits/compare/ax-cpu-v0.6.7...ax-cpu-v0.6.8) - 2026-06-12

### Fixed

- *(ci)* stabilize x86 Starry QEMU timing ([#1245](https://github.com/rcore-os/tgoskits/pull/1245))

## [0.6.7](https://github.com/rcore-os/tgoskits/compare/ax-cpu-v0.6.6...ax-cpu-v0.6.7) - 2026-06-11

### Fixed

- *(axcpu)* support LoongArch user trap recovery

## [0.6.6](https://github.com/rcore-os/tgoskits/compare/ax-cpu-v0.6.5...ax-cpu-v0.6.6) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))

### Fixed

- *(axcpu)* preserve loongarch64 LASX state for Git HTTPS ([#1178](https://github.com/rcore-os/tgoskits/pull/1178))
- *(axcpu-aarch64)* emulate EL0 MRS reads of ID_AA64* feature registers ([#1128](https://github.com/rcore-os/tgoskits/pull/1128))
- *(ci)* switch x86_64 defaults to dynamic platform ([#1024](https://github.com/rcore-os/tgoskits/pull/1024))

### Other

- *(starryos)* add K230 NNCase runtime demo ([#1058](https://github.com/rcore-os/tgoskits/pull/1058))

## [0.6.5](https://github.com/rcore-os/tgoskits/compare/ax-cpu-v0.6.4...ax-cpu-v0.6.5) - 2026-06-03

### Added

- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))
- *(axtask)* add task stack guard page support ([#811](https://github.com/rcore-os/tgoskits/pull/811))

### Fixed

- *(repo)* normalize allocator and RISC-V dependencies ([#1021](https://github.com/rcore-os/tgoskits/pull/1021))
- *(loongarch64)* make userspace LSX usable (preserve FP/LSX state + fix uc_mcontext offset + advertise AT_HWCAP) ([#917](https://github.com/rcore-os/tgoskits/pull/917))
- *(axcpu)* save SP in aarch64 TrapFrame for kprobe correctness ([#887](https://github.com/rcore-os/tgoskits/pull/887))

## [0.6.4](https://github.com/rcore-os/tgoskits/compare/ax-cpu-v0.6.3...ax-cpu-v0.6.4) - 2026-05-22

### Fixed

- *(axvisor)* recover riscv guest memory faults ([#788](https://github.com/rcore-os/tgoskits/pull/788))

### Other

- *(axbacktrace)* use Backtrace::kind() instead of BacktraceReport ([#748](https://github.com/rcore-os/tgoskits/pull/748))

## [0.6.3](https://github.com/rcore-os/tgoskits/compare/ax-cpu-v0.6.2...ax-cpu-v0.6.3) - 2026-05-19

### Other

- updated the following local packages: ax-page-table-multiarch

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/ax-cpu-v0.6.1...ax-cpu-v0.6.2) - 2026-05-15

### Other

- updated the following local packages: axbacktrace

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-cpu-v0.5.5...ax-cpu-v0.6.0) - 2026-04-27

### Added

- *(axvisor)* add loongarch64 qemu support and CI ([#242](https://github.com/rcore-os/tgoskits/pull/242))
- *(ax-sync)* add mutex lockdep and fix Starry atomic-context violations ([#271](https://github.com/rcore-os/tgoskits/pull/271))

### Other

- Unifies breakpoint and debug trap handling across archs ([#244](https://github.com/rcore-os/tgoskits/pull/244))

## 0.2.2

### Fixes

* [Fix compile error on riscv when enable `uspace` feature](https://github.com/arceos-org/ax-cpu/pull/12).

## 0.2.1

### Fixes

* [Pad TrapFrame to multiple of 16 bytes for riscv64](https://github.com/arceos-org/ax-cpu/pull/11).

## 0.2.0

### Breaking Changes

* Upgrade `memory_addr` to v0.4.

### New Features

* [Add FP state switch for riscv64](https://github.com/arceos-org/ax-cpu/pull/2).
* [Add hypervisor support for aarch64](https://github.com/arceos-org/ax-cpu/pull/10).

### Other Improvements

* Export `save`/`restore` in FP states for each architecture.
* Improve documentation.

## 0.1.1

### New Features

* Add `init::init_percpu` for x86_64.

### Other Improvements

* Improve documentation.

## 0.1.0

Initial release.
