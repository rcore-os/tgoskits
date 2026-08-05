# block-io-bench

`block-io-bench` is an operator-facing StarryOS QEMU app for collecting simple
filesystem read/write throughput numbers from the current root block device.

Run the same NVMe-backed workload on every supported QEMU architecture with:

```bash
cargo xtask starry app qemu -t block-io-bench --arch x86_64
cargo xtask starry app qemu -t block-io-bench --arch aarch64
cargo xtask starry app qemu -t block-io-bench --arch riscv64
cargo xtask starry app qemu -t block-io-bench --arch loongarch64
```

The x86_64 SMP matrix uses the same image, NVMe topology, and workload:

```bash
for cpus in 1 2 4; do
  cargo xtask starry app qemu \
    -t block-io-bench \
    --arch x86_64 \
    --qemu-config "qemu-x86_64-smp${cpus}.toml"
done
cargo xtask starry app qemu -t block-io-bench --arch x86_64
```

To exercise the explicit single-queue INTx fallback with the same workload,
run:

```bash
cargo xtask starry app qemu \
  -t block-io-bench \
  --arch x86_64 \
  --qemu-config qemu-x86_64-nvme.toml
```

The app injects a static `/usr/bin/block-io-bench` binary and a small shell
wrapper into a managed Alpine rootfs. The wrapper runs several benchmark rounds
and prints machine-readable log lines:

```text
BLOCK_BENCH_CONFIG path=/root/block-io-bench-app rounds=5 bytes=4194304 block_bytes=4096 fsync=1 io_model=buffered-file write_scope=write-syscalls cache_drop=none diskstats_device=nvme0n1
BLOCK_BENCH_DISKSTATS round=0 phase=write device=nvme0n1 reads=... sectors_read=... writes=... sectors_written=...
BLOCK_BENCH_DISKSTATS round=0 phase=fsync device=nvme0n1 reads=... sectors_read=... writes=... sectors_written=...
BLOCK_BENCH_ROUND op=write round=0 bytes=4194304 elapsed_us=... mib_s=... checksum=...
BLOCK_BENCH_ROUND op=fsync round=0 bytes=4194304 elapsed_us=... mib_s=... checksum=...
BLOCK_BENCH_ROUND op=read round=0 bytes=4194304 elapsed_us=... mib_s=... checksum=...
BLOCK_BENCH_RESULT op=write round=5 bytes=4194304 elapsed_us=... mib_s=... checksum=...
BLOCK_BENCH_RESULT op=fsync round=5 bytes=4194304 elapsed_us=... mib_s=... checksum=...
BLOCK_BENCH_RESULT op=read round=5 bytes=4194304 elapsed_us=... mib_s=... checksum=...
BLOCK_BENCH_APP_PASSED
```

The `BLOCK_BENCH_RESULT` lines report the median elapsed time across all rounds.
`write` measures buffered write syscalls and `fsync` measures durability
separately. The helper does not explicitly drop caches before `read`, but that
does not imply a cache hit: `BLOCK_BENCH_DISKSTATS` identifies which phase
actually reached the block runtime. Linux and StarryOS results must only be
compared when their request/sector deltas describe the same I/O. The default
workload uses five rounds, a 4 MiB file per round, and 4 KiB I/O blocks.
Override these from the QEMU shell environment when needed:

```sh
BLOCK_BENCH_ROUNDS=7 \
BLOCK_BENCH_BYTES=8388608 \
BLOCK_BENCH_BLOCK_BYTES=4096 \
BLOCK_BENCH_FSYNC=1 \
BLOCK_BENCH_PATH=/root/custom-block-io-bench \
/usr/bin/block-io-bench.sh
```

Every architecture uses NVMe with 64 I/O queue pairs and 65 MSI-X vectors.
x86_64 boots eight CPUs; aarch64, riscv64, and loongarch64 boot four. After SMP
startup, x86_64 and aarch64 report one MSI-X hctx and fixed-affinity I/O vector
per online CPU. The current RISC-V FDT PCI host and LoongArch ACPI IORT paths do
not provide MSI routing, so those architectures select a fixed single-queue
INTx mode during initialization. The `qemu-x86_64-nvme.toml` variant advertises
`msix_qsize=1`; the driver must reject incomplete MSI-X resources, unwind them,
and select the same single-queue INTx mask/drain/rearm path. All cases keep
`fsync` enabled and attach the rootfs with a per-drive snapshot.
