# ax-task LTP hackbench baseline

## Purpose

This document fixes the Linux v7.1 PREEMPT_RT and TGOSKits `dev` reference
measurements for scheduler work. Future branches should compare against these
recorded samples instead of rebuilding either baseline. The numbers are valid
only for the protocol and host conditions below; they are not general hardware
performance claims.

## Measurement contract

- Workload: LTP 20260529 `hackbench`, launched through
  `apps/starry/qemu/ltp-hackbench`.
- Arguments: one group, 1,000 loops, and five measured rounds for both process
  and thread modes.
- CPU sets: CPU 0 for the one-CPU case and CPUs 0-3 for the four-CPU case.
- Order: one warm-up per CPU set, followed by alternating 1-CPU/4-CPU measured
  order. Warm-ups are excluded from the medians.
- Virtual machine: QEMU `q35`, TCG `thread=multi`, 512 MiB, four vCPUs, and the
  same NVMe raw root filesystem in snapshot mode.
- Completion contract: exactly one `LTP_HACKBENCH_APP_PASSED` marker and no
  failure marker.
- Host: Linux 6.8.0-124-generic x86_64, 16 logical CPUs, QEMU 10.1.0. No other
  CPU-heavy local test ran during either baseline.

The StarryOS command is:

```sh
set -o pipefail
cargo xtask starry app qemu \
  -t qemu/ltp-hackbench \
  --arch x86_64 \
  --qemu-config qemu-x86_64-benchmark.toml \
  2>&1 | tee target/ltp-hackbench/current-performance.log
```

## Linux v7.1 PREEMPT_RT baseline

- Source commit: `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`
  (`v7.1`).
- Configuration: `CONFIG_PREEMPT_RT=y`, `CONFIG_NR_CPUS=4`,
  `CONFIG_HZ_1000=y`, NVMe built in.

| Mode | CPUs | Samples (seconds) | Median | 4-CPU speedup |
| --- | ---: | --- | ---: | ---: |
| process | 1 | 10.959, 9.882, 10.633, 10.540, 10.313 | 10.540 s | — |
| process | 4 | 6.559, 6.635, 6.456, 6.634, 6.141 | 6.559 s | 1.606x |
| thread | 1 | 4.795, 5.255, 4.993, 5.161, 5.217 | 5.161 s | — |
| thread | 4 | 2.590, 2.402, 2.310, 2.340, 2.609 | 2.402 s | 2.148x |

## TGOSKits dev baseline

- Commit: `21ef4b218ebb74641134238c2050d488c7504249`.
- Completion marker: observed exactly once.

| Mode | CPUs | Samples (seconds) | Median | 4-CPU speedup | vs. Linux RT |
| --- | ---: | --- | ---: | ---: | ---: |
| process | 1 | 54.055, 53.716, 55.443, 56.343, 54.735 | 54.735 s | — | 5.193x slower |
| process | 4 | 53.525, 53.300, 54.410, 55.099, 55.338 | 54.410 s | 1.005x | 8.295x slower |
| thread | 1 | 57.373, 57.373, 65.860, 58.046, 62.808 | 58.046 s | — | 11.247x slower |
| thread | 4 | 58.349, 58.949, 64.654, 58.779, 65.638 | 58.949 s | 0.984x | 24.542x slower |

The `dev` result has effectively no four-CPU scaling in either mode, while the
Linux RT baseline scales by 1.606x and 2.148x. Scheduler changes therefore use
Linux RT as the semantic and scalability reference; merely matching `dev` is
not a success criterion.

## Future comparison rule

Compare all four medians under the exact contract above. A current-build median
above 1.5 times its matching `dev` median is a regression event, not a reason to
extend the timeout:

| Mode | CPUs | 1.5x dev median |
| --- | ---: | ---: |
| process | 1 | 82.102 s |
| process | 4 | 81.615 s |
| thread | 1 | 87.069 s |
| thread | 4 | 88.423 s |

If the QEMU version, acceleration mode, vCPU topology, root filesystem, LTP
version, workload arguments, or host resource conditions change, record a new
named protocol rather than overwriting these baselines.
