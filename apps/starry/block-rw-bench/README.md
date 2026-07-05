# block-rw-bench

Starry board file-I/O benchmark for SD/MMC RDIF validation.

Build and install the helper into the board rootfs as `/usr/bin/block-rw-bench`
before running `cargo xtask starry app board -t block-rw-bench -b <board>`.
The board app `init.sh` executes that helper directly.

The helper writes files under `/root/block-rw-bench/`, calls `sync_all`, reads
the data back, verifies a deterministic pattern, and prints one result line for
each block size plus a final success line.
