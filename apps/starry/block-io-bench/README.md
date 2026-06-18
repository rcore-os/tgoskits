# block-io-bench

`block-io-bench` is an operator-facing StarryOS QEMU app for collecting simple
filesystem read/write throughput numbers from the current root block device.

Run it with:

```bash
cargo xtask starry app qemu -t block-io-bench --arch x86_64
```

The app injects a static `/usr/bin/block-io-bench` binary and a small shell
wrapper into a managed Alpine rootfs. The wrapper runs several benchmark rounds
and prints machine-readable log lines:

```text
BLOCK_BENCH_CONFIG path=/root/block-io-bench-app rounds=5 bytes=4194304 block_bytes=4096
BLOCK_BENCH_ROUND op=write round=0 bytes=4194304 elapsed_us=... mib_s=... checksum=...
BLOCK_BENCH_ROUND op=read round=0 bytes=4194304 elapsed_us=... mib_s=... checksum=...
BLOCK_BENCH_RESULT op=write round=5 bytes=4194304 elapsed_us=... mib_s=... checksum=...
BLOCK_BENCH_RESULT op=read round=5 bytes=4194304 elapsed_us=... mib_s=... checksum=...
BLOCK_BENCH_APP_PASSED
```

The `BLOCK_BENCH_RESULT` lines report the median elapsed time across all rounds.
The default workload uses five rounds, a 4 MiB file per round, and 4 KiB I/O
blocks. Override these from the QEMU shell environment when needed:

```sh
BLOCK_BENCH_ROUNDS=7 \
BLOCK_BENCH_BYTES=8388608 \
BLOCK_BENCH_BLOCK_BYTES=4096 \
BLOCK_BENCH_PATH=/root/custom-block-io-bench \
/usr/bin/block-io-bench.sh
```
