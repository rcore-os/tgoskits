# block-rw-bench

Starry board file-I/O validation for the OrangePi 5 Plus RK3588 DWCMSHC
eMMC path.

Build and install the helper into the board rootfs as `/usr/bin/block-rw-bench`
before running `cargo xtask starry app board -t block-rw-bench -b <board>`.
The board app `init.sh` executes that helper directly.

The helper first verifies that `/` is mounted from `/dev/mmcblk0`. It then runs
512-byte, 4-KiB, ADMA2 maximum-transfer, forced planner-split, and eight-task
concurrent cases. Every case writes files under `/root/block-rw-bench/`, calls
`sync_all`, reads the data back, and verifies a deterministic pattern. Only a
fully successful run prints `ORANGEPI_BLOCK_RW_BENCH_PASSED`.

Each sequential case transfers 8 MiB. The eight concurrent workers transfer
2 MiB each; these sizes keep the depth-one hardware queue validation bounded
while still exercising thousands of 512-byte submissions and every required
ADMA2 split boundary.
