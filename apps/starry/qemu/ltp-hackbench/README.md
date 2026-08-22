# LTP hackbench SMP QEMU app

This manually selected Starry app validates scheduler and SMP behavior on
x86_64, AArch64, RISC-V, and LoongArch with the unmodified LTP `hackbench`
installed by the managed Alpine rootfs. It requires `tgosimages` v0.0.12 or
newer, including these files:

- `/opt/ltp/testcases/bin/hackbench`
- `/opt/ltp/Version`

The prebuild step only compiles the Starry-owned affinity helper and installs
the guest runner. It does not clone, download, build, or patch LTP.

All smoke profiles use four virtual CPUs and an NVMe rootfs. The helper verifies
that four CPUs are online and allowed, then uses raw affinity syscalls and
checks the selected mask before it executes `hackbench`. The x86_64 benchmark
profile additionally fixes multi-threaded TCG and 512 MiB of memory for
repeatable measurements.

## Smoke validation

The default profile is intentionally short. It runs process and thread mode on
one and four CPUs once each with `groups=1` and `loops=10`. It validates the
rootfs contract, topology, affinity, workload execution, and output parsing; it
does not report representative performance.

```bash
for arch in x86_64 aarch64 riscv64 loongarch64; do
  cargo xtask starry app qemu \
    -t qemu/ltp-hackbench \
    --arch "$arch"
done
```

A successful smoke run emits four `LTP_HACKBENCH_SMOKE` records and ends with
exactly one `LTP_HACKBENCH_SMOKE_PASSED`.

## Performance benchmark

Select the x86_64-only benchmark profile explicitly when performance data is
required:

```bash
set -o pipefail
cargo xtask starry app qemu \
  -t qemu/ltp-hackbench \
  --arch x86_64 \
  --qemu-config qemu-x86_64-benchmark.toml \
  2>&1 | tee target/ltp-hackbench/current-performance.log
```

For both process and thread mode, the benchmark performs one warmup and five
measured runs on one and four CPUs. It alternates the measured CPU order and
reports the median plus the informational `one_cpu_median / four_cpu_median`
speedup without enforcing a minimum speedup.

The benchmark defaults are `groups=1`, `loops=1000`, and `rounds=5`. When the
runner is invoked directly in the guest, these can be overridden with
`LTP_HACKBENCH_GROUPS`, `LTP_HACKBENCH_LOOPS`, and `LTP_HACKBENCH_ROUNDS`.
`rounds` must be odd and at least three.

Structured benchmark output includes `LTP_HACKBENCH_SOURCE`,
`LTP_HACKBENCH_TOPOLOGY`, `LTP_HACKBENCH_SAMPLE`, `LTP_HACKBENCH_RESULT`, and
`LTP_HACKBENCH_SPEEDUP`. A complete benchmark ends with exactly one
`LTP_HACKBENCH_APP_PASSED`. Any smoke or benchmark failure emits
`LTP_HACKBENCH_APP_FAILED` and exits nonzero.
