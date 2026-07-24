# Linux vs StarryOS sysbench (OrangePi-5-Plus RK3588, 2026-07-20)

Same board, same binary (glibc `sysbench 1.0.20`), same commands
(`harness/sysbench-compare.sh`). StarryOS = this worktree's kernel with the
ondemand DVFS governor + the axtask spawn/wake distribution fix. Linux = board
Ubuntu 6.1.43-rockchip (native DVFS to 2.4 GHz). Two StarryOS columns show the OPP
ceiling before and after "push the ceiling" (A76 1592→1725 MHz, A55 1372→1523 MHz,
the calibrated safe max at 850→925/950 mV).

> **⚠️ Reproducibility / provenance.** The 8-thread rows below (and "threads now
> use all 8 cores" / "no brownout at threads=8") are a **historical research
> measurement taken on a kernel built with `max_cpu_num = 8`** plus the
> uncommitted DVFS-ceiling and ring-lever `cpufreq.rs` work on `worktree-sysbench`
> (2026-07-20). They are **not reproducible from the config shipped in this PR**:
> `build-aarch64-unknown-none-softfloat.toml` pins `max_cpu_num = 4` on purpose
> (the board browns out / smp-8 boot is unproven at 8 cores on the test PSU, see
> the config header + README "Gating risk"). The submitted `init.sh` therefore
> schedules `--threads=8` on **4** online cores and reproduces the **SMP-4**
> scaling curve, not the 8-core numbers. Treat the 8-thread column as an
> archived result of that specific experimental kernel/config, not as an output
> of this PR's default workflow. To reproduce the 8-core numbers you must build a
> (currently unshipped, brownout-risk) `max_cpu_num = 8` kernel.

## Results

| metric                     | StarryOS @1592/1372 | StarryOS @1725/1523 | Linux   | Linux / StarryOS(top) |
|----------------------------|---------------------|---------------------|---------|-----------------------|
| cpu events/s, 1 thread     | 271                 | 300                 | 974     | 3.2x                  |
| cpu events/s, 2 threads    | 938                 | 1471                | 1954    | 1.33x                 |
| cpu events/s, 4 threads    | 2697                | ~3300               | 3894    | ~1.2x                 |
| **cpu events/s, 8 threads**| 3668                | **3991**            | **5322**| **1.33x**             |
| threads test, events (t=8) | 11339               | 12694               | 50595   | 4.0x                  |
| mutex total time (t=8, s)  | 1.14                | 1.02                | 0.46    | 2.2x                  |
| memory write MiB/s (t=8)   | 11573               | 13347               | 54984   | 4.1x                  |
| memory read  MiB/s (t=8)   | 14161               | 15939               | 55599   | 3.5x                  |

For scale: before this effort StarryOS was **flat at ~160 ev/s** regardless of
thread count (~33x behind Linux at 8 threads). Governor + scheduler fix took it to
3668, and pushing the OPP ceiling to **3991 — 1.33x behind Linux**, from 33x.

## Reading it

- **CPU throughput went from ~33x behind Linux to 1.33x.** Three fixes stacked:
  the DVFS governor (per-core throughput ~2x, 816→1725 MHz on A76), the OPP-ceiling
  push (voltage-calibrated safe max), and the axtask spawn/wake distribution fix
  (flat → 13.6x thread scaling; threads spread across the machine — measured on the
  `max_cpu_num=8` research build, see the provenance note; the shipped SMP-4 config
  spreads across 4 cores).
- **The residual CPU gap is frequency, not compute.** Both OSes reach ~96% of
  their own big.LITTLE aggregate ceiling; per-core compute at equal freq is at
  parity (StarryOS A76@816 == Linux A76@816, established earlier). StarryOS tops
  A76 at 1725 MHz (the calibrated over-volted safe max via SCMI+PVTPLL) vs Linux
  2.4 GHz — 1725/2400 = 0.72, which is the 8-thread gap. The SCMI/PVTPLL coupling
  plus the PSU envelope prevent matching 2.4 GHz on the current voltage lever;
  closing it further needs a different clock path or more PSU headroom.
  StarryOS's boot core is also an A55, hence the larger 1-thread gap.
- **Scheduler-heavy and memory workloads still lag** (threads 4.0x, mutex 2.2x,
  memory ~3.5-4x). Separate levers: context-switch/futex overhead (the scheduler
  effort) and the page-fault / first-touch memory path (roadmap #4), not addressed
  here. Both narrowed slightly with the higher clocks.

## Provenance
Raw captures: `snapshots/linux-vs-starry-2026-07-20/` (Linux + StarryOS @1592) and
the pushed-ceiling run in-line above. Linux baseline matches the earlier RESULTS.md
run (974/1954/3894/5322). cpuprobe `mhz_pmc` (PMU cycle-counter oracle) confirmed
the top OPPs on-board: A76 ~1725, A55 ~1520 MHz, exact, no brownout at threads=8
on that `max_cpu_num=8` research build (the shipped board config caps at SMP-4 for
PSU margin — see the provenance note above).
