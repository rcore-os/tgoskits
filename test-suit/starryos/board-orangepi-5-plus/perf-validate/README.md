# perf-validate — RK3588 SMP + big.LITTLE hardware-PMU board validation

One self-contained C binary (`src/perf_validate.c`) that validates the StarryOS
SMP per-CPU + big.LITTLE `perf` implementation on a real Orange Pi 5 Plus
(RK3588: 4× Cortex-A55 cpu0-3 + 4× Cortex-A76 cpu4-7). It auto-discovers
topology and runs every applicable check, then prints a verdict. The full
validation matrix, expected values, and interpretation live in
`docs/superpowers/perf-board-validation-plan.md`.

## Why the board (what QEMU can't prove)

QEMU `virt` is homogeneous (cortex-a53, every core `ClusterId::Other`). Real
MIDR cluster identity, the dual-PMU `cpus` masks (`0-3`/`4-7`), cross-cluster
ENOENT, the `BRANCH_INSTRUCTIONS` 0x0C-vs-0x21 PMCEID divergence, per-cluster
`PMCR.N`, secondary-PE bring-up, and A76>A55 IPC are **only** observable on
silicon — the QEMU suite could only fake clusters via the parity test-override.

## Run modes (auto-detected)

- **board** — cpu0 MIDR is a real RK3588 core (A55 `0xD05` / A76 `0xD0B`): the
  full real-silicon matrix.
- **selftest** — anything else (auto on QEMU, or `PERF_VALIDATE_SELFTEST=1`):
  enables the parity override and exercises the cluster/pool LOGIC +
  counting/sampling/rdpmc. Silicon-only rows self-SKIP. This is the permanent
  QEMU regression (`qemu-smp4/system/perf-validate`, which holds a byte-identical
  copy of this source) and the pre-board debug. `PERF_VALIDATE_BOARD=1` forces
  board mode.

## Today's status (smp1 boot)

The board boots cleanly only at `max_cpu_num=1` (an smp8 late-boot hang — a
NON-perf bug). So `needs-smp8` / `needs-both-clusters` checks SKIP and the
verdict is **PARTIAL** = "single-core regression anchor passed; big.LITTLE
UNVALIDATED, blocked by the smp8 hang". PARTIAL is a SUCCESSFUL board run today.
When smp8 boots (see `smp8-staged-build-aarch64.toml`), the verdict is **FULL**.

## Deploy + run (board)

The xtask deploys the KERNEL only; the binary must be on the board's ext4 first.

```sh
# 1. Cross-compile a static aarch64 binary (host, via the container):
./deploy.sh build                 # -> ./perf-validate

# 2. With the board in OrangePi Linux (cabled NIC up), deploy it:
./deploy.sh deploy   # stages to /tmp, sudo-installs to /usr/local/bin/perf-validate
#    (override BOARD_USER / BOARD_IP / BOARD_DEST / BOARD_PW as needed)

# 3. Power-cycle into StarryOS and run the board test from the ostool-server host
#    (board OFF at launch, powered ON at the "waiting for power on" cue):
cargo xtask starry test board -c perf-validate \
  -b OrangePi-5-Plus --server localhost --port 2999
```

Success matches `BOARD_PERF_VALIDATE_VERDICT (FULL|PARTIAL)`; the unique final
line `BOARD_PERF_VALIDATE_DONE` lets a hang time out instead of matching early.

### First-run caveats (see board-run-mechanics)

- `BOARD_DEST` is `/usr/local/bin/perf-validate` — on the SD ext4 (mmcblk1p2)
  that StarryOS mounts as `/`, so StarryOS runs it by full path. `/root` is
  700-root and not orangepi-writable; `/usr/local/bin` is the proven shared path
  (the perf 6.6 binary lives there too).
- The board's cabled NIC drifts between the two 2.5G ports (`enP4p65s0` /
  `enP3p49s0`); whichever is UP but only has a `169.254.x` link-local address is
  the live one — add the static IP to it: `sudo ip addr add 192.168.50.2/24 dev
  <live-nic>`. The host NIC `en5` needs `sudo ifconfig en5 192.168.50.1 …`.
- If Linux boot reports ext4 corruption, run a U-Boot fsck repair first (prior
  board tests have left the rootfs needing repair).
- The binary writes `perf_test_force_clusters=0` on exit; in board mode it never
  enables the parity override.

## Self-test under QEMU (pre-board)

```sh
cargo xtask starry test qemu --arch aarch64 -c qemu-smp4/system/perf-validate
# auto selftest mode (parity override); exits 0 on SELFTEST-OK.
```
