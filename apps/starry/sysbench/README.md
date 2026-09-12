# sysbench (StarryOS)

Runs the industry-standard [`sysbench`](https://github.com/akopytov/sysbench)
benchmark on StarryOS. Two goals:

1. Validate the SMP scheduler / big.LITTLE work with a *recognized, genuinely
   parallel* workload that is isolated from the RKNN pipeline (the RKNN workload
   is single-core-serialized and hides scheduler behaviour).
2. Produce Linux-vs-StarryOS numbers on the *same* RK3588 board — a clean,
   apples-to-apples story for the submission.

`sysbench` is installed at runtime from the Alpine **community** repo
(`apk add sysbench` → 1.0.20, LuaJIT-backed), matching the house pattern used by
the other `apps/starry/stress/*` cases. No static cross-compile is needed.

## QEMU — functional validation only

```bash
# 4-thread CPU smoke
cargo xtask starry app qemu -t sysbench --arch aarch64

# cpu(1/2/4 scaling) + threads + mutex + memory matrix
cargo xtask starry app qemu -t sysbench --arch aarch64 \
  --qemu-config qemu-aarch64-matrix.toml
```

On a macOS host with no native `qemu-system`, run these inside the amd64 project
container under OrbStack (see the workspace notes).

> **The QEMU numbers are not performance.** Under TCG (and doubly so when the
> amd64 image is itself emulated on an arm64 Mac) vCPUs are serialized, so the
> CPU scaling curve is flat by construction. QEMU only proves that the
> LuaJIT-linked binary runs and that the sync-heavy `threads`/`mutex` subtests do
> not hang the futex path. **Real performance and scaling come from the board.**

Validated 2026-07-14 under `qemu-smp4` (aarch64, `max_cpu_num=4`): both the smoke
and the full matrix pass (`SYSBENCH_SMOKE_OK` / `SYSBENCH_MATRIX_OK`), with
`sysbench 1.0.20 (using system LuaJIT 2.1.…)` and 4 worker threads scheduled.

## Board — real numbers (next)

A sibling **board** case is the next step. A single case directory cannot hold
both `qemu-*` and `board-*` configs (the app discovery rejects it), so the board
variant lives in its own directory (e.g. `apps/starry/sysbench-board/` with
`init.sh` + `board-orangepi-5-plus.toml`). It runs the same subtests on the
OrangePi-5-Plus and captures a Linux-vs-StarryOS table. The sharpest scheduler
signals are `threads` and `mutex` (sync contention) plus the `cpu` 1→4 scaling
curve across the A76/A55 clusters.

## Files

| file | purpose |
|------|---------|
| `build-aarch64-unknown-none-softfloat.toml` | SMP-4 kernel (`max_cpu_num=4`, nvme root, virtio-net for `apk`) |
| `qemu-aarch64.toml` | 4-thread `sysbench cpu` smoke |
| `qemu-aarch64-matrix.toml` | `cpu` 1/2/4 scaling + `threads` + `mutex` + `memory` |
