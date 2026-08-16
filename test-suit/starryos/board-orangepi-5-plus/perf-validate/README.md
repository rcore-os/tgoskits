# perf-validate — RK3588 SMP + big.LITTLE hardware-PMU board validation

One self-contained C binary (`c/src/perf_validate.c`) that validates the
StarryOS SMP per-CPU + big.LITTLE `perf` implementation on a real Orange Pi 5
Plus (RK3588: 4× Cortex-A55 cpu0-3 + 4× Cortex-A76 cpu4-7). It auto-discovers
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

## Two cases: smp1 anchor + smp8 big.LITTLE gate

This directory (`perf-validate`) is the **single-core anchor**: `max_cpu_num=1`,
full drivers, always-green. Its `needs-smp8` / `needs-both-clusters` checks SKIP
and the verdict is **PARTIAL** = "single-core regression passed; big.LITTLE not
exercised" — a SUCCESSFUL anchor run.

The big.LITTLE behavior is enforced by the sibling **`../perf-validate-smp8`**
case: a minimal `max_cpu_num=8` kernel (drops USB/NPU/PCIe/net, whose
secondary-core IRQ storm causes the smp8 boot hang) that runs the SAME binary and
accepts **ONLY FULL**. PROVEN on real 4×A55+4×A76 silicon (39 pass / 0 fail →
FULL). Splitting the two keeps the anchor stable while ensuring a skipped SMP
path can never pass unnoticed. The smp8 case's `c/CMakeLists.txt` compiles this
directory's `c/src/perf_validate.c`, so both cases build the identical validator
from a single board-side source.

## How the validator reaches the board (session assets, per run)

There is NO manual pre-staging. The validator is built and delivered by the
standard board **session-asset** flow (see `test-suit/starryos/GUIDE.md`), the
same mechanism `native-network-smoke` and the `iperf3` app use:

1. On every `cargo xtask starry test board` run, xtask cross-compiles
   `c/CMakeLists.txt` with the shared musl toolchain (from THIS commit's
   `c/src/perf_validate.c`).
2. The CMake `install(TARGETS perf-validate RUNTIME DESTINATION bin)` product is
   uploaded to a per-run session directory and served to the board as
   `${sessionFile:bin/perf-validate}`.
3. The board `shell_init_cmd` downloads it into `/tmp`, `chmod +x`es it, and
   executes it. A download/chmod/exec failure prints
   `BOARD_PERF_VALIDATE_SETUP_FAILED` and fails the case fast instead of hanging.

So the board always runs the current commit's validator; the self-hosted CI board
runner needs no out-of-band provisioning, and there is no stale-binary risk.

## Run it

```sh
# Board case (self-hosted runner / local board). Board OFF at launch, powered ON
# at the "waiting for power on" cue. The kernel is built + deployed and the
# validator is built + uploaded automatically.
cargo xtask starry test board -c perf-validate --board orangepi-5-plus
cargo xtask starry test board -c perf-validate-smp8 --board orangepi-5-plus
```

Success matches `BOARD_PERF_VALIDATE_VERDICT (FULL|PARTIAL)` (anchor) /
`... FULL` (smp8); the unique final line `BOARD_PERF_VALIDATE_DONE` lets a hang
time out instead of matching early.

### Board caveats (see board-run-mechanics)

- The board's cabled NIC drifts between the two 2.5G ports (`enP4p65s0` /
  `enP3p49s0`); the session HTTP download needs board networking up. Whichever
  port is UP but only has a `169.254.x` link-local address is the live one — add
  the static IP: `sudo ip addr add 192.168.50.2/24 dev <live-nic>` (host `en5`
  side: `sudo ifconfig en5 192.168.50.1 …`).
- If Linux boot reports ext4 corruption, run a U-Boot fsck repair first (prior
  board tests have left the rootfs needing repair).
- The binary writes `perf_test_force_clusters=0` on exit; in board mode it never
  enables the parity override.

## Self-test under QEMU (pre-board)

```sh
cargo xtask starry test qemu --arch aarch64 -c qemu-smp4/system/perf-validate
# auto selftest mode (parity override); exits 0 on SELFTEST-OK.
```
