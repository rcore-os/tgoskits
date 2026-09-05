# Ondemand DVFS governor — board validation (OrangePi-5-Plus RK3588, 2026-07-18)

The `rk3588-cpufreq` driver's dynamic ondemand governor (see
`drivers/ax-driver/src/soc/rockchip/cpufreq.rs` + the `cpufreq-gov` task in
`os/StarryOS/kernel/src/entry.rs`) was validated on the board.

## How freq is read

`cpuprobe <cpu>` (harness) pins to a core, self-loads ~1 s, then times a fixed
compute loop → `ips`. `pmc_ok=0` on StarryOS, so freq is derived from ips vs a
same-binary baseline: **A55 58.26M ips @ 816 MHz, A76 77.69M ips @ 816 MHz**.
cpuprobe's own ~1 s self-load means the 100 ms governor has already scaled the
probed core's cluster **up** by the time it measures — so ips reads the
governor-chosen (loaded) frequency.

## Results

| build | A55 cpu0–3 | A76 cpu4–7 | note |
|---|---|---|---|
| cluster-**average** busy (bug) | ~408 MHz | ~408 MHz | one busy core = 25%/50% of cluster, never crosses 80% up-threshold → never boosts; sits at floor |
| **per-core** + 1608/1416 caps | — | ~1733 MHz | overshoot: ring=1608 @ 762.5 mV over-delivers (undervolt) |
| **per-core** + 675 mV caps (final) | **~1018 MHz** (tgt 1008) | **~1186 MHz** (tgt 1200) | exact, voltage-safe |

- **Up-scaling**: per-core scoring boosts a cluster from its busiest CPU (like
  Linux schedutil/ondemand), so a single CPU-bound thread lifts its cluster.
  After the fix, cpuprobe ips jumped **4.4×** (A76 37.7M → 165M) and sysbench
  `cpu --threads=2` throughput **77 → 295 eps**.
- **Down-scaling**: at idle every cluster decays to the 408 MHz floor (measured
  directly in the average-logic run, where up-scaling never masked it).
- **Exactness**: capping every OPP on the 675 mV rail — the only voltage
  board-proven to make the PVTPLL deliver its SCMI target exactly — removed the
  overshoot. Governor now scales clock 408↔1200 (A76) / 408↔1008 (A55) at fixed
  675 mV; no undervolt, no PMIC voltage changes during scaling.
- **PSU**: `threads=8` all-core ran to completion with **no brownout** (1200/1008
  @ 675 mV draws less than the ~1490 MHz @ 800 mV overshoot the board already
  survived).

## Voltage calibration — higher OPPs unlocked (2026-07-20)

An on-board calibration sweep (gated `CALIBRATE` const in `cpufreq.rs`, run from
`init()` before the console handoff so its `CAL` lines reach serial; measures each
`(ring, voltage)` point's delivered freq via the PMU cycle counter) resolved the
>1200 MHz question. Prereq: the cycle counter is now enabled at boot
(`components/axcpu/src/aarch64/init.rs`), which also makes `cpuprobe`'s `mhz_pmc`
an exact oracle (`pmc_ok=1`).

Key finding: the delivered freq is dominated by voltage; at any DT `(ring=F,
V_nom(F))` pair the delivery *over*-shoots F (e.g. ring 1608 @ 762.5 mV → **1733
MHz** measured, a ~125 mV undervolt). The safe lever is a **fixed low ring with
rising voltage** — the delivered freq climbs while staying over-volted. Measured
ladders (both A76 pairs identical):

| A76 @ ring 1200 | 675→1189 | 725→1318 | 800→1491 | 850→**1592** | 925→1725 |
| A55 @ ring 1008 | 675→1021 | 762.5→1212 | 800→1285 | 850→**1372** | 950→1523 |

Every rung is over-volted (voltage ≥ the delivered freq's DT nominal; margin grows
from ~0 mV at the base to +100 mV at the top). The governor ladders are now HYBRID:
ring-scaled below 1200/1008 (idle floor), voltage-scaled above. New tops (capped at
850 mV, a little above the board-proven 1490 MHz @ 800 mV, below the >1700 @ 925 mV
brownout risk):

- **A76 1200 → 1592 MHz (+33%)**, **A55 1008 → 1372 MHz (+36%)**.

Board-validated 2026-07-20: under load `mhz_pmc` reads **exactly** 1592.x (A76) /
1372.x (A55) — no overshoot; **threads=8 all-core held both with no brownout**;
sysbench eps 201 → 271. Cross-checks (ring 1416 @ 850 → 1830, ring 1608 @ 925 →
2126) undervolt, confirming fixed-ring voltage-scaling is the safe lever.

## Known limitations / follow-ups

- **`gov:` transition logs aren't capturable** post-boot (StarryOS kernel `info!`
  stops reaching the serial once the shell owns the console; during boot the
  cores are busy so no transition fires). cpuprobe freq is the evidence.
- **sysbench eps is flat** t=2 vs t=8 (~201) — a separate StarryOS SMP-scaling
  limitation, orthogonal to DVFS.
