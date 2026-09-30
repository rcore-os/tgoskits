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
board-visible HTTP URL. A networked Starry guest retries that actual transfer
directly; it does not call `ip` to guess readiness. Download or execution
failure is terminal and cannot produce the benchmark success marker.

For a board whose Starry image has no network driver, build the same helper and
connect to the board's default Linux without first booting Starry:

```bash
cargo xtask starry app board -t block-rw-bench \
  --board-config board-visionfive2.toml \
  -b VisionFive2 \
  --linux-stage
```

The command keeps one board lease, uploads the helper through the ostool
session-file API, prints its board-visible `session_file` HTTP URL, and opens
the Linux serial console. In Linux, download that URL and run the helper once
for the comparison row, using the Linux root mount source and a Linux-specific
marker. Then install the exact same file at the persistent handoff path:

```bash
url='<printed session_file URL>'
curl -fsSL "$url" -o /tmp/block-rw-bench
chmod +x /tmp/block-rw-bench
root_device="$(findmnt -n -o SOURCE /)"
BLOCK_RW_BENCH_ROOT_DEVICE="$root_device" \
BLOCK_RW_BENCH_CONTROLLER='<Linux controller name>' \
BLOCK_RW_BENCH_SUCCESS_MARKER='<BOARD>_LINUX_BLOCK_RW_BENCH_PASSED' \
BLOCK_RW_BENCH_MAX_TRANSFER_BYTES='<same limit as board TOML>' \
BLOCK_RW_BENCH_WORKDIR=/root/block-rw-bench-linux \
  /tmp/block-rw-bench
install -D -m 0755 /tmp/block-rw-bench /usr/local/libexec/block-rw-bench
sha256sum /tmp/block-rw-bench /usr/local/libexec/block-rw-bench
sync
```

Exit `board connect` to release the lease, then run the ordinary Starry command
shown above without `--linux-stage`. `init.sh` prefers the Linux-staged helper
and otherwise uses bounded session HTTP retries. Both paths execute the same
Rust binary and require that binary itself to emit the configured success
marker; there is no shell benchmark fallback.

The legacy shared AArch64 kernel build TOML remains available for existing
direct invocations. New board runs should pass an explicit board profile:
axbuild resolves `board-<name>.toml` to
`os/StarryOS/configs/board/<name>.toml`, so boards with the same Rust target do
not accidentally inherit each other's SoC features. The available profiles
cover OrangePi 5 Plus, LicheeRV Nano SG2002, AKA-00 SG2002, VisionFive2,
PhytiumPi, ROC-RK3568-PC DWCMSHC eMMC, Rock 4D RK3576 DWCMSHC eMMC, and
JL-LSGD2K10 AHCI.

Each board profile uses its `shell_cmd` as a parameter prelude with the
expected root device, controller, hardware transfer limit, and unique success
marker. Axbuild injects that prelude before the shared `init.sh`;
the guest does not guess a board from `/proc/device-tree`. The helper verifies
that `/` is mounted from `BLOCK_RW_BENCH_ROOT_DEVICE` and reports
`BLOCK_RW_BENCH_CONTROLLER`. It then runs 512-byte, 4-KiB, hardware
maximum-transfer, forced planner-split, and multi-task
concurrent cases. Every case writes files under `/root/block-rw-bench/`, calls
`sync_all`, reads the data back, and verifies a deterministic pattern. Only a
fully successful run prints the unique marker in `BLOCK_RW_BENCH_SUCCESS_MARKER` (default `BLOCK_RW_BENCH_PASSED`). Sizes, worker count, fsync, and checksum scenario are configurable with `BLOCK_RW_BENCH_SEQUENTIAL_BYTES`, `BLOCK_RW_BENCH_MULTITASK_BYTES_PER_WORKER`, `BLOCK_RW_BENCH_MULTITASK_WORKERS`, `BLOCK_RW_BENCH_FSYNC`, and `BLOCK_RW_BENCH_CHECKSUM_SCENARIO`.

Linux/Starry comparisons must use the same helper hash, root medium, sequential
and worker byte counts, fsync setting, checksum scenario, and maximum-transfer
value. Record CPU count, mount source, controller/driver, helper SHA-256, and
all per-case output; do not compare measurements gathered from different
workloads.

`BLOCK_RW_BENCH_MAX_TRANSFER_BYTES` must be a nonzero 512-byte multiple. The
only supported checksum scenario is `pattern`; unknown values fail instead of
silently running a different workload.

Each sequential case transfers 8 MiB. The eight concurrent workers transfer
2 MiB each; these sizes keep the depth-one hardware queue validation bounded
while still exercising thousands of 512-byte submissions and every required
ADMA2 split boundary.

## Measurement boundary

This benchmark measures the **full file-I/O stack** of the running StarryOS
image: VFS, ext4, page cache and syscalls. Reported per-operation read
latencies are file-interface timings and may be served **entirely by the page
cache**: reads follow a `sync_all` write of the same file, and syncing does
not evict cache lines, so a sample can complete without any request reaching
the block runtime or the hardware controller. Unless
`BLOCK_RW_BENCH_DROP_CACHES` is enabled, the read percentiles must not be
read as device latency.

With `BLOCK_RW_BENCH_DROP_CACHES` enabled, check the per-phase `diskstats`
delta lines printed for each case (`phase=read`): a nonzero `reads=` delta
confirms that the phase reached the device path, and only such a verified run
supports claims about the hardware controller. The startup line reports the
measurement boundary as `boundary=full-file-stack-not-raw-request-latency`.
When saving an individual `case=` or `result case=` line for a Linux/Starry
comparison, preserve that startup line with the case output so the latency
boundary remains explicit.

The latencies are **not** raw block-request latencies under any
configuration, and the numbers must not be read as a sync-versus-async block
submission comparison.

Raw request-level correctness (registration consistency, completion of two
independent asynchronous reads with separate DMA ownership, contiguous and
multi-descriptor write-read integrity, scratch-region restore) is covered by the kernel's
`block_runtime_axtest` regressions. The standard CI `axtest_kernel` entry
discovers and runs that suite, but a board environment without a configured
scratch region makes the write cases self-skip
(`BLOCK_WRITE_TESTS_SKIPPED`): only the read cases execute under plain CI.
The write-read coverage therefore depends on running the suite on hardware
whose kernel command line declares the scratch region
(`axtest.block_scratch=<start_lba>:<blocks>`).
