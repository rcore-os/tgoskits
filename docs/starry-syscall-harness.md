# StarryOS Syscall And Performance Harness

This harness compares small syscall probes on Linux and StarryOS, and runs qperf-based StarryOS performance profiles with structured hotspot reports.

## Usage

Run from the repository root:

```bash
python3 tools/starry-syscall-harness/harness.py doctor
python3 tools/starry-syscall-harness/harness.py discover --arch riscv64
python3 tools/starry-syscall-harness/harness.py perf-profile --arch riscv64 --timeout 20
```

The harness re-enters Docker for StarryOS work by default. It uses:

```text
ghcr.io/rcore-os/tgoskits-container:latest
```

Artifacts are written under:

```text
target/starry-syscall-harness/<arch>/latest/
```

The main report is `report.json`.

Performance artifacts are written under:

```text
target/starry-syscall-harness/perf/<arch>/latest/
```

Important files:

```text
report.json
report.md
hotspots.csv
qperf/stack.folded
qperf/flamegraph.svg
qperf/summary.txt
qperf/qperf.summary.txt
qperf/qemu.time.txt
qperf/qemu.perf.csv
```

qperf profiling defaults to a release StarryOS build so the hotspot data is
closer to optimized runtime behavior. Pass `--debug` only when the additional
debug information is needed for symbol investigation.

By default, qperf samples all translated guest code and maps known StarryOS
kernel physical aliases back to ELF virtual addresses before analysis. Pass
`--kernel-filter` to discard samples outside the detected kernel `.text` range.

Use `perf-diff` to compare two folded stacks or profile directories:

```bash
python3 tools/starry-syscall-harness/harness.py perf-diff \
  --baseline target/starry-syscall-harness/perf/riscv64/baseline \
  --compare target/starry-syscall-harness/perf/riscv64/latest
```

The performance report includes rule-based fix candidates for common VirtIO and lock/copy hotspots. These candidates are intentionally triage-oriented: inspect the referenced files and validate with a new qperf run before treating a change as an optimization.

## qperf Principle And Metric Scope

qperf is implemented as a QEMU TCG plugin under `tools/qperf`. It samples guest
PCs and frame-pointer stacks from QEMU translation-block or instruction
callbacks, then `qperf-analyzer` resolves the guest addresses against the
StarryOS kernel ELF. This answers where the guest kernel is executing; it is not
the same as host `perf` on the QEMU process.

The plugin emits a `qperf.summary.txt` file with sampling health and guest
execution counters:

```text
samples
dropped_samples
sample_failures
translated_blocks
translated_instructions
executed_blocks
executed_instructions
execute_callbacks
```

In `tb` mode, `executed_instructions` is computed as guest instructions in the
translated block multiplied by block executions. In `insn` mode, instruction
callbacks count executed guest instructions directly. These are QEMU guest
instruction counters for the instrumented scope, not hardware retired
instructions.

The current QEMU plugin API does not expose precise guest hardware cycles or
guest cache misses. For that reason, `--host-perf` is explicitly host-scoped:
it measures the host QEMU process, TCG, device emulation, and qperf overhead.
If `perf` is unavailable in the Docker image or the host denies access, the
report records the error instead of inventing counter values.

`--host-time` is always independent of GNU `time`: `cargo xtask starry perf`
records wall time with `Instant` and user/system CPU time with
`getrusage(RUSAGE_CHILDREN)` around the QEMU wrapper.

## Performance CLI Details

The harness entrypoint forwards the following qperf controls:

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 20 \
  --format folded \
  --freq 99 \
  --max-depth 64 \
  --mode tb \
  --top 20 \
  --host-time \
  --host-perf \
  --host-perf-events task-clock,cycles,instructions,cache-references,cache-misses \
  --shell-init-cmd 'echo workload; sleep 1' \
  --shell-prefix 'root@starry:' \
  --qemu-arg=-m \
  --qemu-arg=768M
```

Use `--shell-init-cmd` to profile a real guest workload instead of only boot.
Use repeated `--qemu-arg` entries for raw QEMU arguments; values beginning with
`-` should use the `--qemu-arg=-device` form.

The browser UI exposes the same performance controls and renders summary,
guest-counter, host-time, and host-perf metric groups from `report.json`.

## Codex MCP

Register the MCP server locally:

```bash
codex mcp add starry-syscall-harness -- python3 /path/to/tgoskits/tools/starry-syscall-harness/mcp_server.py --repo /path/to/tgoskits
```

The MCP tools expose `doctor`, `discover`, `perf-profile`, and `perf-diff` flows.
