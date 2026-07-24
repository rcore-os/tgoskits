# Snapshot 0000 — baseline (no optimizations)

The reference rung: upstream StarryOS with none of the four levers implemented.
Every future rung is compared against this.

| field | value |
|---|---|
| rung | 0000 (baseline) |
| date | 2026-07-16 |
| StarryOS commit | `dbbe0e065` (upstream `rcore-os/tgoskits` dev) |
| build config | `max_cpu_num=8`, aarch64, rockchip SoC/SD/eMMC (`apps/starry/sysbench-board/build-aarch64-…toml`) |
| board | OrangePi-5-Plus (RK3588: 4× A76 + 4× A55) |
| Linux reference | Orange Pi Jammy `6.1.43-rockchip`, same board/binary |
| levers enabled | none |
| binary | `sysbench 1.0.20` glibc-dynamic aarch64 (same on Linux + StarryOS) |

## The ladder (this rung)

sysbench cpu (`--cpu-max-prime=20000`), events/sec:

| config | StarryOS | Linux |
|---|--:|--:|
| single-thread (default) | **159** | 977 |
| all-cores perfect placement @ ~800 MHz (measured aggregate) | **2046** | — |
| = Linux single-thread parity target | (977) | 977 |
| = Linux 8-thread parity target | (5320) | 5320 |

**StarryOS today = 159 (1 A55 core @ ~800 MHz). Linux 8-thread = 5320. Gap ≈ 33×.**

## Per-lever headline (measured; see ../../ROADMAP.md)

| lever | measured signal at this rung | potential |
|---|---|---|
| DVFS | all 8 cores stuck ~800 MHz (no cpufreq) | 2.25–2.82× |
| placement | A76 pinned 351 vs A55 160 (affinity works; default never uses A76) | 2.2–2.7× |
| balancer | 4-thread masked to 4 cores still = 1-core (351); aggregate ceiling 2046 | up to 12.9× |
| fault-path | first-touch 256 MB = 0.8–1.3 s vs Linux 0.086 s | ~11×/fault (alloc-heavy) |

## Files

- `linux-harness.out` — Linux reference curves + per-core + matrix (frozen)
- `starry-harness.out` — StarryOS per-core cpuprobe + pinned sysbench + membw + matrix (frozen)
- `decompose.txt` — `decompose.py` output for this rung
