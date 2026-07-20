# Optimization ladder — snapshots

Each subdirectory is one **rung**: a frozen, comparable capture of a full harness
run at a specific StarryOS commit/config, so the sequence of rungs is a clean,
presentable ladder of our optimizations.

A rung is created by: implement a lever (or intra-lever step) → run the harness
(`harness/deploy-harness.sh` → `linux-harness.sh` → boot StarryOS → `decompose.py`)
→ freeze the outputs here with a `MANIFEST.md`.

## Naming

`NNNN-<label>-<starryos-sha>/` — `NNNN` is the ordered rung number, `<label>` names
the lever/step just added (e.g. `0010-dvfs-performance-gov`, `0020-a76-placement`).

## Contents of each rung

- `MANIFEST.md` — date, StarryOS commit + build config, board, levers enabled, the
  ladder numbers, and the per-lever headline.
- `linux-harness.out`, `starry-harness.out` — frozen raw harness outputs.
- `decompose.txt` — the `decompose.py` decomposition for the rung.

## Rungs

| rung | label | StarryOS | StarryOS ev/s (default→ceiling) | % of Linux 8-thr |
|---|---|---|---|--:|
| 0000 | baseline | `dbbe0e065` | 159 → 2046 (ceiling @800MHz) | 3.0% / 38% |

*(Add a row per rung. "% of Linux 8-thread" (5320) is the headline progress metric.)*

> The formal snapshot/ladder system + per-lever benchmark suites are designed in
> `../BENCHMARKING.md` (generated from the design workflow).
