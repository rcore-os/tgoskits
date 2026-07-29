# block-rw-bench

Starry board file-I/O validation with board-specific runtime parameters.

Run a profile with its explicit board config, for example:

```bash
cargo xtask starry app board -t block-rw-bench \
  --board-config board-orangepi-5-plus.toml \
  -b OrangePi-5-Plus
```

Axbuild cross-compiles the static helper from `rust/`, uploads it as an ostool
session file, and expands `${sessionFile:usr/bin/block-rw-bench}` to the
board-visible HTTP URL. The board downloads the helper into `/tmp`; no SSH
deployment or persistent rootfs installation is required.

The app intentionally has no shared kernel build TOML. Axbuild resolves each
`board-<name>.toml` to `os/StarryOS/configs/board/<name>.toml`, so two boards
with the same Rust target cannot accidentally inherit each other's SoC
features. The available profiles cover OrangePi 5 Plus, LicheeRV Nano SG2002,
AKA-00 SG2002, VisionFive2, PhytiumPi, and separate ROC-RK3568-PC
DWCMSHC-eMMC and DWMMC-SD paths.

Each board profile uses its `shell_init_cmd` as a parameter prelude with the
expected root device, controller, hardware transfer limit, and unique success
marker. Axbuild injects that prelude before the shared `init.sh`;
the guest does not guess a board from `/proc/device-tree`. The helper verifies
that `/` is mounted from `BLOCK_RW_BENCH_ROOT_DEVICE` and reports
`BLOCK_RW_BENCH_CONTROLLER`. It then runs 512-byte, 4-KiB, hardware
maximum-transfer, forced planner-split, and multi-task
concurrent cases. Every case writes files under `/root/block-rw-bench/`, calls
`sync_all`, reads the data back, and verifies a deterministic pattern. Only a
fully successful run prints the unique marker in `BLOCK_RW_BENCH_SUCCESS_MARKER` (default `BLOCK_RW_BENCH_PASSED`). Sizes, worker count, fsync, and checksum scenario are configurable with `BLOCK_RW_BENCH_SEQUENTIAL_BYTES`, `BLOCK_RW_BENCH_MULTITASK_BYTES_PER_WORKER`, `BLOCK_RW_BENCH_MULTITASK_WORKERS`, `BLOCK_RW_BENCH_FSYNC`, and `BLOCK_RW_BENCH_CHECKSUM_SCENARIO`.

`BLOCK_RW_BENCH_MAX_TRANSFER_BYTES` must be a nonzero 512-byte multiple. The
only supported checksum scenario is `pattern`; unknown values fail instead of
silently running a different workload.

Each sequential case transfers 8 MiB. The eight concurrent workers transfer
2 MiB each; these sizes keep the depth-one hardware queue validation bounded
while still exercising thousands of 512-byte submissions and every required
ADMA2 split boundary.
