# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Gate architectural user entry on a runtime-owned produced/acknowledged epoch
  after raw IRQ masking, so OS work published during exit-to-user processing is
  either drained before entry or retained behind the scheduler doorbell.
- Preserve ax-task's typed scheduler reasons at timer IRQ return: periodic
  housekeeping, expired timers, and bounded continuations no longer become an
  unconditional preemption request in the runtime adapter.
- Supply backtrace walking with the current runtime stack allocation or exact
  per-CPU boot stack, rather than assuming the entire kernel virtual-address
  range is mapped and readable during a panic.
- Treat the shared hardware IPI as a transport doorbell: acknowledge published
  scheduler epochs on any arrival without converting the interrupt itself into
  a preemption request.
- Route logs through a one-shot runtime output sink after early-console
  handover, and stream panic diagnostics through a bounded emergency callback
  plus one shutdown-only bounded transmitter drain, without allocation,
  sleeping locks, or fallback to the retired UART owner.
- Limit the worker-service topology to the boot CPU when the final image has
  SMP disabled, even if firmware describes additional physical CPUs.
- Materialize every logical device in a controller bundle after initialization,
  keep queue selection device-scoped, and retain recovery and passthrough as
  controller-wide lifecycle transitions.
- Drive staged block-controller initialization on the shared high-priority
  workqueue after binding IRQ actions, and publish queues only after Ready.
- Retain deferred initialization IRQ sources in a fixed bitmap and acknowledge
  them in the bounded worker before advancing the controller state machine.
- Replace destructive multi-controller detach with typed prepare, commit,
  quarantine, guest-return, and host-running ownership permits. Retain driver
  queues and IRQ registrations across passthrough for proof-gated recovery.
- Publish delayed-work deadline and generation as one atomic command so
  concurrent task/IRQ producers cannot pair one update's ordering token with
  another update's deadline.
- Keep synchronous delayed-work cancellation behind the expiration publication
  baton until an old timer generation is either restored and cancelled or its
  queued activation is visible to `cancel_work_sync`.
- Republish the delayed-work cancel command on every baton retry so one
  concurrent deadline modification cannot leave synchronous cancellation
  repeatedly re-arming the replacement timer.
- Linearize every delayed-work schedule or deadline modification through the
  logical queue's packed admission state; a new timer retains that reservation,
  while an existing timer releases its temporary permit only after publishing
  the replacement command, so drain cannot admit a late deadline extension.
- Keep synchronous delayed-work flushing behind the same expiration publication
  baton so it cannot snapshot an idle user item before the timer path queues it.
- Order intrusive incoming publication and the worker doorbell in one
  sequentially consistent handshake so a concurrent producer is either
  detached by the consumer or leaves a pending doorbell; a cross-atomic weak
  ordering can no longer lose both indications.
- Consume detached MPSC snapshots one node at a time instead of reversing an
  unbounded list, and reject caller-selected passes above the fixed 64-node
  latency budget.
- Add a task-context logical-workqueue drain boundary that closes admission and
  waits for every previously accepted item without tearing down shared workers.
- Pack logical-workqueue admission, active-item count, and drain state into one
  atomic transition, and defer the final drain wake through a fixed worker item
  so a hard-IRQ reservation rollback never scans a task-context wait queue.
- Validate synchronous drain context before closing logical queue admission, so
  an IRQ or shared-worker misuse returns without destructively starting drain.
- Keep OS IRQ acknowledgement actions live until device-side masking commits
  during activation rollback, recovery, and passthrough handoff.
- Keep a failed block IRQ's complete backing line quenched until recovery has
  successfully masked the device source and explicitly releases the owning
  action, so a shared line cannot spin on an asserted failed source.
- Detach drained host IRQ actions from their descriptors into linear callback
  tokens before guest ownership, then reattach them disabled after guest route
  revocation and before IRQ-capable controller reinitialization.
- Separate completion and watchdog claims so only returned DMA ownership can
  publish a timed-out request as terminal.
- Quarantine the DMA backing carried by a stale or duplicate driver completion
  instead of dropping memory whose hardware ownership epoch cannot be proven.
- Serialize request cancellation through the hctx worker: staged ownership is
  returned directly, while in-flight cancellation waits for DMA-quiesced
  controller recovery before publishing its terminal result.
- Split block controller, hctx, and passthrough logic by owned lifecycle,
  request-table, service-loop, resource-identity, and transaction invariants.
- Use one directory module entry for workqueue state, runtime workers, and
  delayed-timer fragments instead of splitting the parent entry from its files.
- Keep inline software queues preemption-pinned without disabling interrupts
  across their synchronous memory-copy submission path.
- Keep controller, hctx, request-table, and recovery worker locks
  preemption-safe while leaving hardware IRQ delivery enabled; hard IRQ paths
  communicate only through preallocated atomic event bridges.
- Keep inline and activation completion sinks fixed-capacity, release queue
  locks before returning request ownership, and quarantine a driver that emits
  more terminal completions than one bounded hctx pass can retain.
- Charge IRQ continuation work before deciding whether to call the driver
  again, and defer a full completion batch or remaining event-ring snapshots
  to the next shared-worker pass.

## [0.10.4](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.10.3...ax-runtime-v0.10.4) - 2026-07-10

### Added

- *(msi)* add hierarchical MSI-X irq domains ([#1526](https://github.com/rcore-os/tgoskits/pull/1526))

## [0.10.3](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.10.2...ax-runtime-v0.10.3) - 2026-07-08

### Fixed

- *(platforms)* route DMA cache sync through platform cache ops ([#1542](https://github.com/rcore-os/tgoskits/pull/1542))

## [0.10.2](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.10.1...ax-runtime-v0.10.2) - 2026-07-08

### Other

- updated the following local packages: ax-plat, ax-alloc, axklib, ax-driver, ax-hal, ax-ipi, ax-mm, ax-task, ax-display, ax-fs-ng, ax-net, ax-input

## [0.10.1](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.10.0...ax-runtime-v0.10.1) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, ax-task, dma-api, rd-net, aic8800, axfs-ng-vfs, rdrive, ax-plat, ax-alloc, axklib, rdif-block, ax-driver, ax-hal, ax-ipi, ax-mm, ax-display, ax-fs-ng, ax-log, ax-net, ax-input

## [0.10.0](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.9.0...ax-runtime-v0.10.0) - 2026-07-07

### Added

- *(starfive-jh7110-dwmmc)* add IRQ-driven host ([#1524](https://github.com/rcore-os/tgoskits/pull/1524))
- *(msi)* add aarch64 MSI-X registration ([#1522](https://github.com/rcore-os/tgoskits/pull/1522))

### Fixed

- *(block)* drive virtio-blk completions by IRQ ([#1512](https://github.com/rcore-os/tgoskits/pull/1512))

### Other

- Remove `ax-feat` crate and redistribute features across runtime, API, and user library layers ([#1513](https://github.com/rcore-os/tgoskits/pull/1513))
- remove static platform and axconfig generation, make dynamic platform the only path ([#1478](https://github.com/rcore-os/tgoskits/pull/1478))

## [0.9.0](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.8.2...ax-runtime-v0.9.0) - 2026-07-02

### Added

- *(axtest)* add ArceOS QEMU smoke coverage ([#1365](https://github.com/rcore-os/tgoskits/pull/1365))

### Fixed

- *(ci)* prevent Starry qemu hangs in IRQ paths ([#1431](https://github.com/rcore-os/tgoskits/pull/1431))
- *(irq)* close domain runtime review gaps

### Other

- *(ax-driver)* remove static platform compatibility ([#1463](https://github.com/rcore-os/tgoskits/pull/1463))
- *(irq-framework)* require boxed IRQ callbacks ([#1452](https://github.com/rcore-os/tgoskits/pull/1452))
- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))
- *(net)* split IRQ handlers from NIC queues ([#1435](https://github.com/rcore-os/tgoskits/pull/1435))
- *(somehal)* modernize x86 qemu irq routing ([#1430](https://github.com/rcore-os/tgoskits/pull/1430))
- *(build)* generate build.rs Rust sources with quote ([#1422](https://github.com/rcore-os/tgoskits/pull/1422))
- *(ax-runtime)* resolve device IRQ bindings to IrqId

## [0.8.2](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.8.1...ax-runtime-v0.8.2) - 2026-06-27

### Added

- *(ax-runtime)* generate banner build info ([#1373](https://github.com/rcore-os/tgoskits/pull/1373))

### Other

- *(platform)* remove ax-config from dynamic runtime path ([#1387](https://github.com/rcore-os/tgoskits/pull/1387))
- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.8.1](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.8.0...ax-runtime-v0.8.1) - 2026-06-23

### Fixed

- *(platform)* support AArch64 HVF timer boot ([#1334](https://github.com/rcore-os/tgoskits/pull/1334))

### Other

- *(ax-net)* add locking and concurrency documentation and remove deprecated interfaces ([#1340](https://github.com/rcore-os/tgoskits/pull/1340))

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.7.0...ax-runtime-v0.8.0) - 2026-06-22

### Added

- *(ax-runtime)* prefer UEFI RTC on dynamic platform ([#1294](https://github.com/rcore-os/tgoskits/pull/1294))
- *(ax-net)* add multi-interface support with per-interface routing, DNS, and SO_BINDTODEVICE ([#1244](https://github.com/rcore-os/tgoskits/pull/1244))
- runtime Wi-Fi AP/STA mode switch for AIC8800 on SG2002 (LicheeRV Nano) ([#1266](https://github.com/rcore-os/tgoskits/pull/1266))
- *(axruntime)* add compiler-backed stack protector support ([#1239](https://github.com/rcore-os/tgoskits/pull/1239))
- AIC8800 Wi-Fi SoftAP for SG2002 (LicheeRV Nano) ([#1185](https://github.com/rcore-os/tgoskits/pull/1185))

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.6.2...ax-runtime-v0.7.0) - 2026-06-12

### Added

- *(ax-driver)* add dynamic platform rtc support ([#1242](https://github.com/rcore-os/tgoskits/pull/1242))
- *(irq)* enhance IRQ request handling and state restoration logic
- *(axruntime)* add runtime IRQ registration adapters

### Fixed

- *(starry)* reprogram timer for short deadlines ([#1250](https://github.com/rcore-os/tgoskits/pull/1250))
- *(ci)* stabilize x86 Starry QEMU timing ([#1245](https://github.com/rcore-os/tgoskits/pull/1245))
- *(axruntime)* ensure aarch64 SMP IPI readiness before app init ([#1196](https://github.com/rcore-os/tgoskits/pull/1196))

### Other

- *(ax-net)* unify network stack into single net/ax-net crate, r… ([#1203](https://github.com/rcore-os/tgoskits/pull/1203))

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.6.1...ax-runtime-v0.6.2) - 2026-06-11

### Other

- updated the following local packages: ax-alloc, ax-driver, ax-config, ax-hal, ax-mm, ax-task, ax-fs-ng, ax-plat, axklib, ax-ipi, ax-display, ax-fs, ax-input, ax-net-ng, ax-net

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.6.0...ax-runtime-v0.6.1) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))
- *(backtrace)* add showcase workflow ([#1094](https://github.com/rcore-os/tgoskits/pull/1094))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.5.16...ax-runtime-v0.6.0) - 2026-06-03

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))
- *(axtask)* add task stack guard page support ([#811](https://github.com/rcore-os/tgoskits/pull/811))

### Fixed

- *(axvisor)* enable buddy-slab allocator ([#974](https://github.com/rcore-os/tgoskits/pull/974))
- *(axruntime)* initialize the page allocator from the largest free RAM region ([#922](https://github.com/rcore-os/tgoskits/pull/922))
- *(axruntime)* park secondary harts beyond MAX_CPU_NUM instead of panicking ([#919](https://github.com/rcore-os/tgoskits/pull/919))

### Other

- *(platform)* migrate riscv64 qemu to dynamic platform ([#1085](https://github.com/rcore-os/tgoskits/pull/1085))
- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(axruntime)* remove alloc feature, make it unconditional ([#985](https://github.com/rcore-os/tgoskits/pull/985))
- *(starry)* route HAL access through ax-runtime ([#963](https://github.com/rcore-os/tgoskits/pull/963))
- *(driver)* move static probes to platform-owned registration ([#937](https://github.com/rcore-os/tgoskits/pull/937))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))
- *(axbuild)* use target JSON specs for kernel builds ([#839](https://github.com/rcore-os/tgoskits/pull/839))
- Refactor workspace structure and update dependencies ([#864](https://github.com/rcore-os/tgoskits/pull/864))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.5.15...ax-runtime-v0.5.16) - 2026-05-22

### Other

- *(axbacktrace)* use Backtrace::kind() instead of BacktraceReport ([#748](https://github.com/rcore-os/tgoskits/pull/748))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.5.14...ax-runtime-v0.5.15) - 2026-05-19

### Other

- updated the following local packages: ax-alloc, ax-driver, ax-task, ax-net, axklib, ax-hal, ax-mm, ax-display, ax-fs, ax-fs-ng, ax-input, ax-ipi, ax-net

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.5.13...ax-runtime-v0.5.14) - 2026-05-15

### Other

- updated the following local packages: axbacktrace, ax-alloc, ax-config, ax-hal, ax-mm, ax-fs, ax-fs-ng, ax-log, ax-net, ax-net, ax-plat, ax-driver, ax-task, ax-display, ax-input, ax-ipi

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.5.11...ax-runtime-v0.5.12) - 2026-04-27

### Added

- *(axvisor)* add loongarch64 qemu support and CI ([#242](https://github.com/rcore-os/tgoskits/pull/242))

### Other

- *(ax-alloc)* fix percpu slab spelling
