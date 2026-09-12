# StarryOS speed roadmap (RK3588), from measured sysbench data

StarryOS is **~24–33× behind Linux** on this board — but the harness proved that
is **not an efficiency defect**. Per-core compute is at parity with Linux (same
core, same clock → same throughput). The entire gap is **four independent policy
gaps**, each separately measured, each a distinct subsystem you can fix. Fixing
them **stacks to Linux parity**.

## The ladder (all numbers measured on-board, sysbench cpu ev/s)

| stage | ev/s | vs today | what changed |
|---|--:|--:|---|
| **Today** (default, unpinned) | **159** | 1× | 1 thread on 1 A55 core @ ~800 MHz |
| + DVFS (single thread → max clock) | 360 | 2.3× | A55 @ 800 → 1800 MHz |
| + placement (single thread → A76) | 977 | 6.1× | = **Linux single-thread** |
| + balancer @ 800 MHz (all 8 cores) | **2046** | 12.9× | measured aggregate, no DVFS |
| + balancer **and** DVFS (all cores, max clock) | **~5348** | 33.6× | = **Linux 8-thread (5320)** — parity |

Single-thread path `159 →(DVFS)→ 360 →(A76)→ 977` and full-board
`159 →(balancer)→ 2046 →(DVFS)→ 5348` both land on Linux. Nothing else is missing.

## The four levers, prioritized

Ranked by ROI = (impact × breadth) ÷ effort. "Product" = the RKNN/tennis inference
pipeline (latency-bound, mostly serial); "throughput" = parallel/server workloads.

### 1. DVFS / cpufreq  — **highest ROI, do first**
- **Measured impact:** 2.25× (A55) to 2.82× (A76), *on every core, for every
  workload*. Today all 8 cores are pinned at the ~800 MHz U-Boot boot OPP — StarryOS
  has no cpufreq at all.
- **Breadth:** global multiplier. Helps product **and** throughput, every core.
- **Effort:** medium, self-contained — an RK3588 clock/PLL + regulator driver and a
  governor (even a fixed "performance" governor that sets max OPP is most of the win).
  No scheduler changes.
- **Verify:** `cpuprobe` reports ~2256 MHz on A76 / 1800 MHz on A55; single-thread
  sysbench ~360 (A55) / ~977 (A76).

### 2. big.LITTLE placement  — **best product win, moderate effort**
- **Measured impact:** 2.2× at current clock (A76 351 vs A55 160), 2.7× at max
  clock. Affinity **works** (`taskset` lands correctly) — StarryOS just never puts
  work on the A76 cores by default. Matches the earlier RKNN "pin-to-A76 = 3×".
- **Breadth:** every latency-sensitive single task — **directly the RKNN inference
  path**.
- **Effort:** low–medium. Can ship as a placement heuristic (spawn/prefer big cores
  for CPU-heavy tasks) *before* a full balancer; or expose affinity so the app pins
  itself.
- **Verify:** an unpinned single-thread task runs on cpu4–7 and hits ~A76 numbers.

### 3. Load balancer (spread + migrate)  — **biggest throughput number, most work**
- **Measured impact:** up to **12.9×** at 800 MHz (2046 vs 159), more with DVFS.
- **Key finding:** StarryOS does **not** distribute threads across cores *even when
  given a multi-core affinity mask* — a 4-thread job masked to 4 A76 cores still runs
  at 1-core speed (351). So this is a genuine **work-distribution / stealing**
  gap, not a default-mask tweak. This is the axtask load-balancer effort.
- **Breadth:** parallel/throughput workloads; the product benefits only where its
  pipeline stages run concurrently.
- **Effort:** high (core scheduler work).
- **Verify:** unpinned `sysbench --threads=8` approaches the 2046 (800 MHz) / ~5348
  (with DVFS) aggregate.

### 4. Page-fault / first-touch path  — **fixes the "200× memory", narrower scope**
- **Measured impact:** StarryOS warm memory bandwidth is fine (memcpy 7.3 GB/s A76,
  3.2 GB/s A55; sysbench memory at 1 MB blocks = 2.9 GB/s). The scary sysbench "42
  MiB/s" is the **1 KB-block** test hitting a slow fault path: first-touch of 256 MB
  takes **0.8–1.3 s vs Linux 0.086 s** (~11× slower *per fault*).
- **Breadth:** allocation-heavy / first-touch-heavy phases — e.g. RKNN buffer setup,
  memory benchmarks.
- **Effort:** medium (mm) — fault-around / prefaulting, `MAP_POPULATE`, transparent
  huge pages.
- **Verify:** `membw firsttouch` drops toward Linux; sysbench memory 1 KB-block rises.

## Recommended sequencing

- **For the RKNN/tennis product (latency):** #2 placement → #1 DVFS → #4 fault-path.
  (Placement + DVFS alone take a single inference thread from A55@800 to A76@2256 =
  ~6× before any scheduler rewrite.)
- **For generic OS throughput / benchmark parity:** #1 DVFS → #3 balancer.
- **#1 DVFS is on both critical paths** and is the most self-contained — start there.

## Re-measuring progress

This harness is the regression metric. After each lever, re-run
`deploy-harness.sh → linux-harness.sh → boot StarryOS → decompose.py`; the
decomposition table shows the ladder rung you just climbed. `HS_PC landed==req`,
`HS_PSB` per-core, and `membw firsttouch` are the specific gauges for placement,
DVFS, and the fault path respectively.
