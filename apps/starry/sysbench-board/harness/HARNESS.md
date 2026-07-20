# sysbench decomposition harness

Turns "StarryOS is ~24× slower than Linux" into **directly measured, named
factors** instead of inferences. Built to answer the questions left open by the
first board run:

1. Is StarryOS's boot core really an **A55**, or an A76 at a low clock? *(measure
   MIDR directly; don't infer from throughput.)*
2. What **actual frequency** does StarryOS run it at? *(measure via the cycle
   counter, or map throughput onto a per-core Linux frequency curve.)*
3. Does pinning to an A76 core (`taskset`) actually **reach** it, or does the
   scheduler ignore affinity? *(report the core we actually landed on.)*
4. What causes the **200× memory** gap — a real memory path problem or a sysbench
   artifact? *(a dedicated bandwidth microbench.)*

## Why "same-core + same-frequency" is the only fair per-core comparison

The default StarryOS-vs-Linux number conflates three independent variables:
**core type** (A55 vs A76), **frequency** (StarryOS has no DVFS; Linux ramps to
max), and **parallelism** (load balancing). Pinning *alone* doesn't isolate OS
efficiency — you must also control frequency. This harness measures each factor
separately so the gap decomposes as `DVFS × placement × balancing`, and the true
per-core comparison (same core, same clock) can be read off directly.

## Instruments

- **`cpuprobe [core]`** — pins to a core, then reports: `landed` (the core it
  actually ran on → did affinity work?), `part` (MIDR core-type: `0xd05`=A55,
  `0xd0b`=A76), `ips` (CNTVCT-timed throughput), and `mhz_pmc` (exact MHz from the
  PMU cycle counter, when the kernel exposes EL0 access). The MIDR/PMU reads are
  fork+signal isolated, so unsupported reads degrade to `midr_ok=0`/`pmc_ok=0`
  rather than crashing.
- **`membw [core] [MiB]`** — first-touch (page-fault) cost + warm memcpy/read
  bandwidth over a large buffer.

Both are glibc-dynamic aarch64 (libc deps only, like sysbench). Build with
`build-harness.sh` (native arm64 Ubuntu container; verified compiling + running).

## Run procedure

```bash
# 0. build instruments (host)
bash build-harness.sh

# 1. board in Linux — deploy + capture the reference curves
SB=../../../tmp/sysbench-static/sysbench-glibc-aarch64 bash deploy-harness.sh
ssh orangepi@169.254.50.2 'bash -s' < linux-harness.sh | tee linux-harness.out

# 2. boot StarryOS (server-less) — the uboot-config runs starry-harness.sh
env -u RUSTUP_TOOLCHAIN cargo xtask starry uboot \
  -c ../build-aarch64-unknown-none-softfloat.toml \
  --uboot-config ../uboot-orangepi-5-plus.toml \
  | tee starry-harness.out           # ends at SYSBENCH_BOARD_DONE

# 3. decompose (host)
python3 decompose.py linux-harness.out starry-harness.out
```

`decompose.py --selftest` runs the whole pipeline on the 2026-07-15 measured
numbers so you can see the expected output shape without a board.

## Output tags

| tag | meaning |
|---|---|
| `HL_CORE cpuN part=` | Linux per-core MIDR (ground-truth core map) |
| `HL_REF cl= curkhz= ips= sb=` | reference point: cpuprobe ips + sysbench ev at a set frequency, per cluster |
| `HL_MX` / `HS_MX` | sysbench matrix (cpu/threads/mutex/memory) |
| `HS_PC` | StarryOS per-core cpuprobe (type/freq/affinity) |
| `HS_PSB c= ev=` | StarryOS sysbench pinned to core c |
| `HS_PM` / `HL_PM` | membw per core |
| `HS_MEMSW` | memory sweep (block size × oper) |

## What "confirmed" will look like

- `HS_PC` shows `part=0xd05` on the boot core ⇒ A55 confirmed by MIDR (not inferred).
- `HS_PC ... mhz_pmc≈800` (or `HS_PSB` mapped via `HL_REF`) ⇒ actual clock confirmed.
- `HS_PSB c=4` ≈ `c=0` **and** `HS_PC req=4 landed=0` ⇒ affinity ignored → placement
  is blocked by the scheduler, not by hardware. If instead `c=4` jumps ~2.7× and
  `landed=4`, the A76 works and placement is purely a default-policy choice.
- `membw` collapsing on StarryOS ⇒ real memory-path issue; tracking Linux/freq ⇒
  the sysbench-memory number was a config artifact.
