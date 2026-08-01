# Native Zephyr real-time baseline

This app supplies the native/equivalent RTOS comparison required by
[`competition/requirement.md`](../../requirement.md). It runs upstream Zephyr
v4.3.0 directly on QEMU's single-core `qemu_cortex_a53` board. It does not boot
AxVisor and must not be presented as hypervisor or cross-guest evidence.

## Measurement contract

The benchmark creates a highest-priority cooperative task and wakes it from an
absolute periodic `k_timer` every 1 ms. It discards 100 warm-up expirations and
collects exactly 10,000 measured expirations (10 seconds nominal). No console
output occurs while those samples are being collected. The AArch64 architected
timer backs `k_cycle_get_64()` and runs at 62.5 MHz on this board.

Two non-negative latency distributions are reported in nanoseconds:

- `periodic_wake_lateness`: task observation time minus the timer's absolute
  tick deadline. This includes QEMU timer delivery, Zephyr's timer ISR and
  scheduler dispatch.
- `timer_to_task_dispatch`: task observation time minus a cycle timestamp at
  the start of the timer expiry callback. This focuses on callback-to-thread
  wake and dispatch cost.

Each distribution reports count, minimum, integer mean, nearest-rank p50, p90,
p99, p99.9, and maximum. The app also reports actual measured duration, the
number of expirations coalesced into an already-pending task wake, and Zephyr
runtime-accounted CPU ownership. Coalesced deadlines remain in the fixed sample
set with their actual task observation time, so host stalls are visible in the
tail instead of silently disappearing. `timer_misses` counts only coalescing
among the 10,000 measured expirations; `warmup_timer_misses` reports the 100
discarded expirations separately. The corresponding maximums are 10,000 and
99: one wake can represent the first warm-up expiration while all measured
expirations are already pending, but that represented wake is not itself a
miss.

The stress build keeps a preemptible priority-5 xorshift worker continuously
runnable. The periodic task uses `K_PRIO_COOP(0)`, so the stress worker is
strictly lower priority. A run is accepted only when its work counter and
runtime-accounted CPU share both show sustained load (at least 900 permille).
The idle build does not create the worker, verifies zero stress activity, and
requires at least 900 permille idle time. These thresholds distinguish the two
cases using measured execution rather than the selected configuration alone.

## Contract tests

The host-side tests exercise the warm-up/measurement boundary directly and
reject evidence whose source-provenance index metadata does not match the named
Git index file:

```sh
bash competition/rt-baseline/zephyr/tests/run.sh
```

These tests do not boot QEMU and are safe to run independently of a timing
campaign.

## Reproduce

The repository-local validation environment places the exact upstream source,
west environment, and Linux-target AArch64 compiler shim under ignored `tmp/`
paths. Install `git`, CMake, Ninja, Python venv support, `qemu-system-aarch64`,
and the Ubuntu `gcc-aarch64-linux-gnu`/`binutils-aarch64-linux-gnu` packages,
then prepare and run it with:

```sh
bash competition/rt-baseline/zephyr/prepare.sh
bash competition/rt-baseline/zephyr/run.sh all
```

The retained run used west 1.5.0, QEMU 10.0.3, and Ubuntu's
`aarch64-linux-gnu-gcc` 11.4.0 behind that shim. From WSL or another Linux
shell, run both scripts at the repository root. `prepare.sh` pins both the
annotated tag object and its peeled commit and refuses a dirty source tree.

The default output is:

```text
tmp/competition/rt-baseline/zephyr/
  source-provenance.json
  idle/{build.log,qemu.log,summary.json,build/}
  cpu-stress/{build.log,qemu.log,summary.json,build/}
```

An alternative output root can be supplied as the second argument; relative
paths are resolved from the repository root. Individual cases are `idle` and
`cpu-stress`. Evidence is immutable: the runner refuses an
existing case directory and the analyzer refuses an existing output file, so a
failed rerun cannot leave a stale success summary next to new logs. Use a new
output root for every campaign. All selected QEMU cases finish before one
post-run `source-provenance.json` clean-tree scan; each analyzer reuses that
bounded attestation while rechecking its tag, commit, and Git index. This avoids
repeated multi-minute worktree scans on NTFS without moving the scan into the
measured path. The analyzer rejects missing result records,
wrong sample counts or duration, non-monotonic percentiles, unverified load,
early timer callbacks, a source tree other than the exact upstream v4.3.0 tag,
and any `RTOS_BASELINE_FATAL` marker. It retains the coalesced-expiration count
as a result rather than treating host scheduling interference as invalid data.

The tag is annotated. For upstream v4.3.0, its tag object must be
`981205b3e7cdf9fdf2e9e71b8b6b64fcc71c12a0` and its peeled commit must be
`3568e1b6d5cdd51a6b964a2a1d6d29200fea2056`; the analyzer enforces both.
Use the Zephyr SDK's AArch64 GCC/binutils prefix, or provide a compatible
freestanding `CROSS_COMPILE` prefix.

The retained Windows-hosted checkout contains CRLF working-tree files. Its
checkout-local `core.autocrlf=true` setting makes Git compare those files with
the canonical LF blobs, and the post-run status is clean. No Zephyr source file
was rewritten to obtain the attestation. A normal WSL/Linux clone may leave
`core.autocrlf` unset and produces the same pinned Git objects.

For a standard Zephyr workspace and Zephyr SDK compiler, override the
dependency paths instead:

```sh
ZEPHYR_BASE=/path/to/zephyr-v4.3.0 \
WEST=/path/to/venv/bin/west \
WEST_WORKSPACE=/path/to/zephyrproject \
CROSS_COMPILE=/path/to/aarch64-zephyr-elf- \
bash competition/rt-baseline/zephyr/run.sh all ./native-zephyr-results
```

`CROSS_COMPILE` is the full prefix before `gcc`; the matching binutils must be
available under the same prefix. Using an official Zephyr SDK is preferred
outside the captured local environment.

## Comparison boundary

This is an equivalent, not identical, comparison with the standalone Linux
guest campaign implemented by `scripts/benchmark/axvisor-rt/run.sh`:

| Property | Native baseline | Standalone AxVisor RT runner |
| --- | --- | --- |
| Software stack | Zephyr directly on QEMU | Linux periodic probe inside AxVisor |
| Virtual CPUs | one | four host pCPUs; two Linux vCPUs dedicated to pCPUs 2 and 3 |
| CPU model | Cortex-A53 | Cortex-A72 |
| QEMU clock mode | normal architected timer; `CONFIG_QEMU_ICOUNT=n` | normal TCG timer |
| Stress placement | lower-priority Zephyr thread on the measured CPU | Linux stress is pinned to guest CPU 1 while probes use guest CPU 0 |
| Hypervisor/other guests | absent | AxVisor is present; this standalone runner has no second guest |
| Timing primitive | Zephyr absolute `k_timer` + cycle counter | Linux absolute sleep/timerfd + `CLOCK_MONOTONIC` |

The native result is therefore a control for RTOS scheduling without
virtualization overhead, not a numerical claim that the two timer or stress
paths are identical. Host contention, QEMU version, CPU model, virtual CPU
topology, runtime accounting overhead, and the different guest kernels can all
change the distribution. Compare trends and worst-case sensitivity, and retain
the per-run tool versions, hashes, duration, and measured load from
`summary.json` alongside every AxVisor result. The full Linux + Zephyr IVC
topology is a separate campaign with an additional guest and virtio-net path;
do not conflate its platform effects with the standalone RT runner.
