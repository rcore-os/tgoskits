# Competition multi-RTOS baseline design

Status: implemented and validated on QEMU/AArch64

Scope: the tracked AxVisor RTOS guest configurations and their reproducibility
contract

## Problem and users

The competition requires at least one native RTOS comparison for the AxVisor
real-time work and awards additional credit for multiple RTOS baselines.  The
repository initially had a reproducible Zephyr v4.3.0 baseline, while the
pre-existing RT-Thread and FreeRTOS VM TOML files only described image
placement. A TOML
file or a third-party binary with no source identity, workload contract, or raw
evidence is not a defensible baseline.

The primary users are:

- developers comparing AxVisor/Linux latency tails against a native RTOS;
- reviewers reproducing the idle and stressed measurements from clean sources;
- maintainers extending the benchmark without weakening its evidence gates.

## Decision

Add two native QEMU/AArch64 baselines alongside Zephyr:

| RTOS | Upstream boundary | Platform boundary |
| --- | --- | --- |
| RT-Thread | tag `v5.2.2`, commit `ddf52e2cdd977f14fc04035c88672ac204aec713` | upstream `qemu-virt64-aarch64` BSP |
| FreeRTOS | `freertos-over-bao` commit `cb9112f982c2768872536b811e013254d0184811`, which pins FreeRTOS-Kernel `f1043c49d59944353291654c175852bd17b34f99` | Bao bare-metal runtime `c50068084212ef33115a4c05f9f714cc637f30bc`, platform `qemu-aarch64-virt` |

Both run directly on QEMU `virt` with one Cortex-A53 and no AxVisor.  They use
the AArch64 architected counter, a 1 kHz RTOS tick, a 1 ms periodic workload,
100 discarded warm-up expirations, and 10,000 retained samples.  Each RTOS has
an idle build and a continuously runnable lower-priority CPU-stress build.

The repository owns only the benchmark application, deterministic
configuration overlay, preparation/build runner, analyzer, and documentation.
Third-party source remains in an ignored `tmp/` checkout and is accepted only
at the pinned commits with a clean-worktree attestation.

## Meaning of support

Support is reported in explicit levels so that guest boot and task-two network
interoperability are not inferred from a native benchmark:

1. **Native baseline**: pinned source builds, both workloads terminate, and the
   analyzer accepts their evidence.
2. **AxVisor guest boot**: the same owned application is built for a declared
   VM memory/interrupt layout and reaches a bounded ready marker under AxVisor.
3. **IVC/1 endpoint**: the RTOS has an IP stack and virtual NIC integration,
   passes the IVC/1 wire vectors, and completes the control/reliability
   campaigns.

The native-baseline change itself targets level 1 for both RT-Thread and
FreeRTOS. Neither an old VM configuration nor a successful native baseline is
described as IVC support. A separate, additive implementation now validates
levels 2 and 3 for both RTOSes on QEMU/AArch64; its resource contract, network
adapters, limitations, and evidence are documented in
[`competition-rtos-guest-ivc.md`](competition-rtos-guest-ivc.md). The native
measurement evidence remains independent and is not relabelled as guest data.

## Measurement contract

The periodic callback records the counter at timer/interrupt context and wakes
the highest-priority benchmark task.  The task records its observation counter
before doing other work.  For measured expiration `i`:

```text
deadline(i) = first_deadline + i * counter_frequency / 1000
wake_lateness(i) = task_observation(i) - deadline(i)
dispatch(i) = task_observation(i) - callback_observation(i)
```

All arithmetic is checked for an integral 1 ms counter step.  A callback or
task timestamp earlier than its deadline is fatal.  When multiple expirations
are pending before the task runs, every represented deadline remains in the
sample set and the coalesced count is reported; latency tails are not discarded.
No console output occurs during the measured window.

The load record is based on RTOS scheduler accounting where available, not on
the selected build option alone. RT-Thread classifies the current thread in its
tick hook because the v5.2.2 generic CPU tracer does not classify kernel idle
time on this BSP; FreeRTOS uses its 64-bit run-time counters. A stress run must
prove that the lower-priority task executed throughout the window. An idle run
must prove that no stress task was created and that the idle task owned the
expected majority of the CPU. RTOS-specific counter granularity is retained in
metadata.

## Evidence contract

Every case emits the existing machine-readable markers:

- `RTOS_BASELINE_CONFIG`
- `RTOS_BASELINE_WORKLOAD_READY`
- two `RTOS_BASELINE_RESULT` records
- `RTOS_BASELINE_LOAD`
- `RTOS_BASELINE_COMPLETE`

Any `RTOS_BASELINE_FATAL` record invalidates the run.  The analyzer additionally
rejects a wrong RTOS/version/platform, missing or duplicate records, the wrong
sample count, non-monotonic percentiles, a short duration, early wakes, an
unverified workload, source drift, a dirty source tree, and missing build/raw
artifacts.  Evidence directories are immutable: runners and analyzers refuse
to overwrite an existing case or summary.

The retained JSON includes raw-console, build-log, configuration, ELF, binary,
and source-provenance hashes.  Tool versions, host kernel/CPU, QEMU command,
counter frequency, priority mapping, and scheduler-accounting method are part
of the result.

## Alternatives considered

### Reuse only the existing VM TOML files

Rejected.  They contain placeholder image paths and do not define a comparable
periodic workload, source provenance, or measurement evidence.

### Use prebuilt RTOS images

Rejected.  A prebuilt image cannot establish the exact kernel configuration,
compiler, benchmark source, or clean upstream identity required for
reproduction.

### Port FreeRTOS directly from the Xilinx A53 demo

Rejected for this baseline.  The official A53 kernel port is reusable, but the
demo depends on a Xilinx BSP and interrupt-controller environment.  The pinned
Bao runtime already provides the small QEMU `virt` startup, GICv3, PL011, and
architected-timer boundary while keeping FreeRTOS-Kernel as an official pinned
submodule.

### Treat QEMU results as an RK3588 bare-metal comparison

Rejected.  QEMU is an equivalent software platform, not the Orange Pi 5 Plus.
Documentation and score claims must keep that limitation visible.  A physical
RK3588 native port and campaign require separate board-specific evidence.

## Compatibility and isolation

The new paths do not change AxVisor, StarryOS, the IVC/1 wire protocol, or the
Zephyr baseline.  Their output uses the same record names but includes an
explicit `os` and version field.  Per-RTOS analyzers enforce their own priority,
timer, and scheduler-accounting details instead of pretending those mechanisms
are identical.

All builds run in ignored output trees.  Preparation scripts refuse unexpected
source identities and do not patch a user's arbitrary checkout.  A failed run
cannot overwrite retained evidence.

## Validation plan

1. Host contract tests for record parsing, percentile and duration gates,
   source identity, immutability, and miss accounting.
2. Warning-clean application compilation with the pinned AArch64 bare-metal
   toolchain.
3. Native QEMU idle and CPU-stress runs for RT-Thread and FreeRTOS.
4. Analyzer acceptance of all four logs and negative tests that mutate one
   contract field at a time.
5. Existing Zephyr baseline contract tests and competition IVC tests remain
   green.

## Rollback

The feature is additive.  Removing the two new baseline directories and their
documentation/index entries restores the previous behavior; no on-disk or wire
format migration is required.
