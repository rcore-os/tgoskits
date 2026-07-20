# sysbench: StarryOS vs Linux on OrangePi-5-Plus (RK3588)

Same `sysbench 1.0.20` glibc binary, same board, same ext4 rootfs. Native Linux
(Orange Pi Jammy, `6.1.43-rockchip`) vs StarryOS booted via serial FIT loady.

- Date: 2026-07-15
- StarryOS: upstream `rcore-os/tgoskits` dev @ `dbbe0e065`, `max_cpu_num` 4 and 8
- Raw logs: `tmp/board-results/` (linux-baseline.out, starry-smp4-final.log,
  starry-boot-upstream-smp8.log, starry-smp8-cpuinfo2.log)

## CPU (events/sec, higher = better)

| threads | StarryOS smp4 | StarryOS smp8 | Linux | Linux / StarryOS |
|--------:|--------------:|--------------:|------:|-----------------:|
| 1 | 159.6 | 159.0 | 981 | ~6.2× |
| 2 | 160.1 | 159.9 | 1949 | ~12× |
| 4 | 160.5 | 160.4 | 3904 | ~24× |
| 8 | — | — | 5333 | (Linux uses all 8) |

Scaling 1→4: **StarryOS 1.01× (flat)** vs **Linux 3.98× (near-linear)**.

**smp8 == smp4, exactly.** Doubling `max_cpu_num` from 4 to 8 (all cores up,
`nproc=8`, no PSU brown-out) produced identical numbers — because with no load
balancer, every thread stays on the boot core. mutex-4 = 7.44s and memory-4 =
41.9 MiB/s were also identical at smp4 and smp8.

## Other subtests

| test | StarryOS smp4 | Linux |
|------|--------------:|------:|
| threads-4 (events) | 1088 | 49358 |
| mutex-4 (total time, lower=better) | 7.45 s | 0.46 s |
| memory-4 (MiB/s, higher=better) | 42 | 8333 |

## Findings

1. **smp4 boots cleanly on upstream.** An earlier `max_cpu_num=4` hang at the
   post-mount fs sync was a *stale-base artifact* — reproduced only on the
   `oscomp-posad` fork base (168 commits behind upstream, missing the axtask
   SMP-wake #1495/#1426 and block-IRQ #1512 fixes). On upstream dev it boots,
   mounts the ext4 root, and reaches the shell with `nproc=4`.

2. **The load balancer is THE bottleneck — smp8 proves it.** smp4 and smp8 give
   *identical* numbers. Bringing up all 8 cores (incl. the A76 big cores) buys
   nothing, because with no load-balancing / work-stealing every sysbench thread
   stays on the boot core. Flat 1→4 scaling + zero smp4→smp8 delta both point at
   the same missing axtask load balancer. More cores ≠ more throughput until it
   exists.

3. **Compute is stuck on the A55 boot core.** Single-thread ~159 ev/s is
   A55-class (the RK3588 boot core is a little core); Linux runs the same binary
   on an A76 @ 2.4GHz (~981) → the ~6× single-thread gap. StarryOS's `/proc/cpuinfo`
   reports a uniform `0xd05` (A55) for all cores — a reporting stub, not literal —
   but the performance confirms work never reaches the A76 cores. This is the
   big.LITTLE placement gap (cf. the "L1 pin-to-A76 = 3×" finding).

4. **Memory bandwidth is pathologically low** (42 vs 8333 MiB/s) — beyond what
   A55 + single-core explains; worth a separate look (allocation / fault-per-block
   path).

## MEASURED decomposition (harness, 2026-07-16) — supersedes the inferences above

The harness (`harness/`, see HARNESS.md) measured directly what was previously
inferred. Same board; StarryOS smp8 @ upstream. Linux reference: A55 0.199 ev/s/MHz
(max 359 @ 1.8 GHz), A76 0.430 ev/s/MHz (max 979 @ 2.256 GHz).

Per-core pinned sysbench on **StarryOS** (affinity `landed`==`req` on all 8 cores):

| StarryOS core | type | sb ev/s | ⇒ frequency |
|--|--|--:|--|
| cpu0–3 | A55 | ~160 | ~800 MHz (≈ Linux A55@816 = 161) |
| cpu4–7 | A76 | ~351 | ~816 MHz (≈ Linux A76@816 = 347) |

**Confirmed, not inferred:**
1. **All 8 cores run at ~800–816 MHz** — no DVFS (every core matches the Linux
   @816 MHz point). The boot core is an A55.
2. **Affinity WORKS and the A76 cores are reachable** — `taskset -c 4..7` lands on
   an A76 (`landed`==`req`) and runs **2.2× faster** (351 vs 160). *This corrects the
   earlier worry that affinity might be ignored.* The default scheduler just never
   migrates work there.
3. **Per-core compute is at parity** on BOTH clusters (A55 160≈161, A76 351≈347 at
   the same clock) — no StarryOS execution penalty.
4. **The 200× memory result is NOT broken memory.** `membw` warm bandwidth is
   7.3 GB/s (A76) / 3.2 GB/s (A55); sysbench memory at **1M blocks = 2.9 GB/s**. The
   42 MiB/s only appears at sysbench's default **1K blocks**, and `firsttouch` is
   0.8–1.3 s on StarryOS vs 0.086 s on Linux ⇒ the cost is a **slow page-fault /
   first-touch path** + per-small-op overhead, not DRAM bandwidth.

**Decomposition (measured):** `159 (A55@~800MHz) × 2.25 (DVFS) × 2.72 (A55→A76 at
max) × 4.0 (1→4 cores) ≈ 24×`. Three independent, addressable levers:
- **DVFS / cpufreq** — cores stuck at the ~800 MHz U-Boot OPP.
- **big.LITTLE placement** — affinity works, but nothing auto-migrates to A76.
- **load balancing** — nothing auto-spreads across cores (unpinned stays on 1 A55).

(cpuprobe's direct MIDR/PMCCNTR reads returned `midr_ok=0`/`pmc_ok=0` — StarryOS
doesn't expose those at EL0 — so core-type/frequency came from the Linux MIDR map +
the frequency curves rather than a direct register read; the cross-checks agree.)

## Why this matters

sysbench cleanly *quantifies* exactly the two levers the kernel-half optimization
effort targets: **big.LITTLE placement** (get work onto A76) and the **load
balancer** (distribute threads across cores). Combined, StarryOS is ~24× behind
Linux at 4 threads today; both gaps are addressable and independently measurable
with this same workload — a ready-made regression/ää progress metric.
