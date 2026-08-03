# AxVisor real-time benchmark harness

This directory contains reproducible measurement assets for QEMU AArch64 and
the OrangePi-5-Plus StarryOS board profiles. It intentionally contains no
captured latency values. A run writes raw output, provenance metadata, and a
derived JSON summary under `tmp/axvisor-rt/`, `competition/results/`, or a
caller-selected output directory.

The final shared/partitioned idle, CPU-stress, and partitioned-soak artifacts
are retained separately in
[`competition/results/axvisor-rt-reference`](../../../competition/results/axvisor-rt-reference/).
That comparison is a same-source policy-off/policy-on experiment and has mixed
tail results; it must not be described as universal latency improvement.

## Measurement boundaries

The guest probe records three nanosecond-valued metrics after a configurable
warmup:

- `periodic_jitter`: lateness of an absolute `clock_nanosleep` periodic task.
- `dispatch_latency`: time from an `eventfd` signal to execution of a woken,
  higher-priority reader pinned to the same guest CPU. It includes the guest
  kernel wakeup and scheduler dispatch path.
- `emulated_irq_response`: time from an absolute `timerfd` deadline to the
  userspace reader resuming. It is a guest-visible virtual-timer
  IRQ-to-userspace proxy, not a direct interrupt-handler or hypervisor IRQ
  injection measurement.

The probe buffers samples and prints them after each measured loop so serial
output is outside individual latency intervals. The physical-board StarryOS
flow additionally records virtual-timer injection and guest IRQ entry in
preallocated fixed rings, with no allocation, lock, or printing in the hot
path. `analyze_irq_trace.py` pairs both sides in the guest virtual-counter
domain and rejects drops, incomplete records, injection failures, and counter
frequency mismatches. The QEMU-only runner does not claim this direct metric.

`--workload cpu-stress` starts the same static probe in a non-real-time busy-loop
mode pinned to guest CPU 1 while all measured probes remain pinned to guest CPU
0. Probe output is captured in the guest, and the runner forwards exact
`AXVISOR_RT_WORKLOAD_READY` and `AXVISOR_RT_WORKLOAD_STOPPED` records containing
the observed PID and CPU. It verifies `/proc/<pid>/status` before and after the
measurement window, explicitly sends `SIGTERM`, waits for the probe, and accepts
the stop record only after that termination. The analyzer checks that the same
PID and CPU appear throughout, that `STOPPED` follows all measurements, and
that CPU 1 was at least 50% busy between the paired CPU snapshots. Thus a
successful `cpu-stress` capture proves sustained in-guest load rather than
trusting a metadata label or an unobserved background process.
It does not prove CPU partition isolation: the busy loop runs on guest CPU 1 of
the same VM, whose vCPU remains pinned to the same pCPU in both profiles. Use an
independently placed and observed interference source for an isolation claim.
Before any workload branch, the guest runner also requires exactly two online
Linux CPUs and emits `AXVISOR_RT_GUEST_CPUS`. This prevents an idle run with a
failed secondary-vCPU boot from being accepted merely because CPU 0 produced
all requested samples. It also records paired `AXVISOR_RT_CPUSTAT` snapshots
for every online guest CPU around the complete measurement window. The analyzer
rejects missing, duplicate, out-of-range, or regressing counters and reports
per-CPU busy ticks and busy percentages. This supplies observed load
distribution in addition to the stress process's affinity evidence.

QEMU is pinned to multi-threaded TCG for repeatability. TCG values support
relative comparisons between otherwise identical runs; they are not hardware
real-time guarantees. `dedicated_cpus` excludes reserved pCPUs from other
registered shared guest-vCPU tasks. It does not automatically exclude ordinary
AxVisor host tasks, housekeeping, or physical interrupts.

## Run

Prerequisites are `qemu-system-aarch64`, `debugfs`, Python 3, the normal AxVisor
build dependencies, and either `aarch64-linux-musl-gcc` or a prebuilt static
AArch64 copy of the probe. Pull the normal QEMU AArch64 image first, then pass
the Alpine rootfs image without modifying it. The exact 718,296-byte probe used
by the retained campaign is
[`competition/results/axvisor-rt-reference/axvisor-rt-probe`](../../../competition/results/axvisor-rt-reference/axvisor-rt-probe),
with SHA-256
`8b3f6e7471dc9ecf60d5b64ab5f3c3a4657af8743fde1aa6b1772358c62806da`:

```sh
cargo xtask image pull --arch aarch64

scripts/benchmark/axvisor-rt/run.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img \
  --probe competition/results/axvisor-rt-reference/axvisor-rt-probe \
  --profile partitioned \
  --iterations 10000 \
  --warmup 100 \
  --period-us 1000 \
  --workload idle
```

The runner copies the rootfs, builds and injects the probe plus a run script,
uses `docs/realtime/axvisor-qemu-aarch64-partition.toml`, captures the complete
console log, and records environment provenance. Before QEMU starts, metadata
is atomically written with status `capture_running` and SHA-256 hashes for the
input rootfs, injected pre-run rootfs, AxVisor/VM/QEMU configurations, probe,
guest runner, repository commit, complete tracked binary diff, and untracked
source manifest. Finalization preserves all of those pre-run fields and adds
only the finish time, QEMU exit/status, and raw-log hash. An interrupted run
therefore remains `capture_running` and cannot be mistaken for completed
evidence. The runner refuses to overwrite an existing result artifact.

The repository fingerprint asks Git for collapsed untracked roots and expands
only those roots itself. It prunes `.git`, `target`, `tmp`, `build`, `build-*`,
`__pycache__`, `competition/results`, and `docs/competition`; tracked changes
remain covered by `git diff --binary HEAD`. This keeps generated build and
evidence trees out of provenance without hiding tracked edits. Use the retained
artifact with `--probe` for a byte-identical probe input. The producing compiler
version was not separately captured, so no compiler-version provenance is
claimed for that binary; if it is rebuilt, record the compiler version and new
probe hash.

Use `--workload cpu-stress` for the harness-controlled CPU 1 load. Use
`--workload external:<safe-label>` only when a separately arranged load was
active and independently verified; the guest log marks that mode as
caller-verified. Unknown labels are rejected. Labels may contain letters,
digits, dots, underscores, and hyphens.

`--profile partitioned` enables the selected dedicated-pCPU configuration.
`--profile shared` selects the compatibility configuration in which both guest
vCPUs may run on any of the four host CPUs. Running the same idle and
`cpu-stress` cases under both profiles is the reproducible feature-off/feature-on
configuration comparison; it is not presented as a historical binary
comparison or proof of isolation under independent contention. Keep the rootfs,
probe, QEMU version, sample count, period, and workload identical between paired
captures.

## StarryOS physical-board path

The board path uses `build-starry-kernel.sh`, `build-starry-rootfs.sh`,
`stage-starry-board.sh`, and `harvest-starry-board.sh`. Raw samples and the
compressed guest IRQ trace are written to the guest block image; the AxVisor
shell writes the host trace and a synchronized snapshot before Linux recovery.
The harvester checks the snapshot read-only, extracts raw evidence, and runs
both StarryOS and direct-IRQ analyzers.

The normal `axvisor-orangepi-5-plus-starry-{shared,partitioned}.toml` profiles
remain single-guest baselines. Experimental cross-VM interference is isolated
behind `axvisor-orangepi-5-plus-starry-noise-{shared,partitioned}.toml`. Both
noise profiles use the same bounded `aarch64-rt-noise.bin`; only its singleton
pCPU placement differs (`pCPU1` for shared contention, `pCPU3` for isolated
placement).

That cross-VM profile is diagnostic-only at present. OrangePi-5-Plus testing
showed that a multi-pCPU noise-vCPU affinity can fail after migration, while
two singleton-pinned AArch64 vCPUs time-sliced on pCPU1 can still produce a
current-EL data abort after the second vCPU starts. A non-preemptible guest run
slice removes the earlier nested-vCPU panic but does not close this correctness
gap. Do not use these failed logs as latency samples or run the partitioned half
as if it formed a valid pair. Either validate a pinned AxVisor host-noise task,
or fix same-pCPU vCPU switching with deterministic and physical regressions
before resuming cross-VM comparison.

The maintained controlled-interference path now uses the single-guest
`starry-host-noise-{shared,partitioned}` profiles. Their top-level
`[host_noise]` configuration starts the same bounded busy-loop task before the
default VM, enables round-robin host scheduling, and stops the task only after
the VM exits. The shared task is singleton-pinned to pCPU1 with StarryOS vCPU0;
the partitioned task is singleton-pinned to pCPU3. Both use the same 180-second
safety bound. A valid run must retain `AXVISOR_RT_HOST_NOISE` and
`AXVISOR_RT_HOST_NOISE_PCPU` in the host trace, prove that the observed mask is
the requested singleton mask, and prove that the noise window covers the full
VM trace. `observed_wall_ticks` is coverage wall time on the observed pCPU; it
is deliberately not labeled as exclusive task CPU runtime. The independent
host pCPU/vCPU accounting remains the authority for contention analysis.

Use the matching board gate and tell the harvester which placement is expected.
The board gate has one terminal success marker, `AXVISOR_SNAPSHOT_SYNC_OK`;
host-noise completion is deliberately validated later from the persisted trace.
Require that sync marker before the wrapper may cold-cycle back to Linux:

```bash
ORANGEPI_AXVISOR_BUILD_CONFIG=scripts/benchmark/axvisor-rt/config/axvisor-orangepi-5-plus-starry-host-noise-shared.toml \
ORANGEPI_AXVISOR_BOARD_CONFIG=scripts/benchmark/axvisor-rt/config/board-orangepi-5-plus-starry-host-noise-shared.toml \
ORANGEPI_AXVISOR_SHUTDOWN_MARKER_REQUIRED=1 \
ORANGEPI_RESTORE_LINUX=1 \
bash competition/ivc/orangepi/board-runner.sh

ORANGEPI_RT_EXPECTED_HOST_NOISE_PCPU=1 \
  scripts/benchmark/axvisor-rt/harvest-starry-board.sh
```

Use the partitioned build/board configs and expected pCPU3 for the paired half.
Harvest immediately after each half because the next board run replaces
`/home/rt`. A lossless host trace must explicitly report
`unowned_virtual_timer_irqs=0` in addition to zero dropped, incomplete, failed
injection, and counter-frequency-mismatch counts. Any nonzero value invalidates
the run; the hard-IRQ path must not print synchronously.

Do not replace the repository wrapper with a direct `cargo xtask axvisor board`
invocation on the local automation host. The wrapper performs the Linux-side
SSH reboot needed to expose U-Boot, applies the temporary U-Boot 2025.10
`uboot-shell` compatibility patch, owns the serial lease, and restores Linux
with the board-specific TF-card boot command before verifying a writable
`/dev/mmcblk1p2` ext4 root filesystem. The local patch can be removed after
drivercraft/ostool PR 164 is released as `uboot-shell` 0.2.7.

## Analyze and test

Raw records use the stable `AXVISOR_RT_SAMPLE schema=1 key=value...` format.
The analyzer validates every field, rejects duplicate or arithmetically
inconsistent samples, requires all three complete iteration ranges, and uses
deterministic nearest-rank percentiles. With metadata, it additionally requires
one start and completion marker, no failure marker, an online-CPU count matching
the configured vCPU count, workload-specific activation and cleanup evidence,
paired `/proc/stat` snapshots for every guest CPU, a successful QEMU exit, and
a raw-log SHA-256 matching the exact analyzed bytes. Metadata validation is
fail-closed and dependency-free: the analyzer requires the exact schema-v1
object structure, a completed status, nonempty identifiers and timestamps,
64-digit SHA-256 values, and every input/config/probe/runner/log artifact.
Unknown fields, missing provenance, and interrupted `capture_running` records
are rejected:

```sh
python3 scripts/benchmark/axvisor-rt/analyze.py \
  tmp/axvisor-rt/<run>/raw-console.log \
  --metadata tmp/axvisor-rt/<run>/metadata.json \
  --output tmp/axvisor-rt/<run>/summary.json

python3 -m unittest discover \
  -s scripts/benchmark/axvisor-rt/tests \
  -p 'test_*.py'

bash scripts/benchmark/axvisor-rt/tests/test_runner.sh
```

`metadata.schema.json` is the JSON Schema for captured provenance.
`metadata.example.json` is explicitly marked `planned` and has no artifact or
measurement values; it is a template, not a benchmark result.
