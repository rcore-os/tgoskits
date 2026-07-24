# sysbench (StarryOS on OrangePi-5-Plus board)

Runs `sysbench` under StarryOS on the RK3588 board and produces the real
performance + scaling numbers that QEMU cannot (see `../sysbench/README.md` — the
QEMU TCG curve is flat by construction). Pairs with a native-Linux run of the
**same binary on the same board** for an apples-to-apples comparison.

This is the board sibling of the QEMU case `apps/starry/sysbench/`. It is a
separate directory because a single app case cannot hold both `qemu-*` and
`board-*` configs (the discovery rejects it).

Selected as `-t sysbench-board`.

## Why the deploy step exists

`cargo xtask starry app board` deploys **only the StarryOS kernel**. Userspace
binaries must already be on the board's ext4 rootfs (`mmcblk1p2`). StarryOS runs
against that same rootfs, and it executes glibc dynamic binaries (cf. the
`glibc-dynamic-smoke` app), so an `apt install sysbench` on the board Linux is
exactly what StarryOS then runs. The board Linux is Ubuntu/`6.1.43-rockchip`, not
Alpine — there is no `apk` here, unlike the QEMU case.

## Procedure

The board loop is **Linux (deploy + baseline) → power OFF → ostool → power ON
(StarryOS run)**. Full board mechanics (ostool-server, `RUSTUP_TOOLCHAIN` trap,
power-on cue, TFTP-vs-loady) live in the board-run notes; the essentials:

1. **Board in Linux** — stage sysbench, then capture the Linux baseline.

   **Deploy — option A (static binary, recommended, no board internet):** build a
   fully static aarch64 sysbench on the host and scp it in. This runs identically
   under Linux and StarryOS and avoids the shared-library-resolution risk that a
   glibc-dynamic (apt) sysbench carries under StarryOS's loader.
   ```bash
   bash build-static-sysbench.sh          # -> ./sysbench-static-aarch64 (confirm `file` says statically linked)
   scp sysbench-static-aarch64 orangepi@192.168.50.2:/tmp/sysbench
   ssh orangepi@192.168.50.2 '
     printf orangepi | sudo -S install -m755 /tmp/sysbench /usr/bin/sysbench
     printf orangepi | sudo -S sync
     /usr/bin/sysbench --version'
   ```

   **Deploy — option B (apt, only if the board has its own internet):**
   ```bash
   ssh orangepi@192.168.50.2 'bash -s' < deploy-sysbench.sh    # apt install + sync
   ```

   Then capture the native-Linux baseline (same binary, same board):
   ```bash
   ssh orangepi@192.168.50.2 'bash -s' < linux-baseline.sh | tee linux-baseline.out
   ```
   The `sync` after deploy is mandatory either way — the root mounts `commit=600`,
   so an unsynced binary is invisible to StarryOS after the re-mount (`not found`).

2. **Power the board OFF** (ostool power is a no-op; it must be off when ostool
   launches, powered on at the "Waiting for remote board to power on…" cue).

3. **Run StarryOS** (note `env -u RUSTUP_TOOLCHAIN` — the shell exports a stable
   toolchain that breaks the `-Z` nightly build):
   ```bash
   env -u RUSTUP_TOOLCHAIN cargo xtask starry app board \
     -t sysbench-board -b OrangePi-5-Plus --server localhost --port 2999
   ```
   Success sentinel: `SYSBENCH_BOARD_DONE`. Capture serial to a file and diff the
   `CPU_THREADS=` / `THREADS_T4` / `MUTEX_T4` / `MEMORY_T4` lines against
   `linux-baseline.out`.

## Gating risk — multi-core StarryOS boot

`max_cpu_num = 4` is set so sysbench can scale, but multi-core StarryOS boot on
this board is unproven (the scheduler effort's smp8 boot hang; the board also
browns out at 8 cores on the current PSU). If StarryOS hangs at boot with smp4:
drop `max_cpu_num` to 1 in the build config for a functional single-core run and
rely on the Linux-side scaling curve, **or** treat bringing smp4 up cleanly as
part of the scheduler work — a clean smp4 sysbench scaling curve here is a strong
result for that effort.

Sharpest scheduler signals once multi-core boots: the `cpu` 1→4 scaling curve and
the sync-heavy `threads` / `mutex` subtests, ideally with cluster affinity
(A76 vs A55) as a follow-up.

## Files

| file | purpose |
|------|---------|
| `build-aarch64-unknown-none-softfloat.toml` | board StarryOS kernel (rockchip SoC/SD/eMMC, `max_cpu_num=4`) |
| `init.sh` | StarryOS-side workload → `SYSBENCH_BOARD_DONE` |
| `board-orangepi-5-plus.toml` | ostool board config (sentinel, fail regexes, timeout) |
| `build-static-sysbench.sh` | host: build a static aarch64 sysbench (deploy option A) |
| `deploy-sysbench.sh` | run on board Linux: `apt install sysbench` + `sync` (deploy option B) |
| `linux-baseline.sh` | run on board Linux: native baseline for the same subtests |
