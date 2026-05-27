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

## Codex MCP

Register the MCP server locally:

```bash
codex mcp add starry-syscall-harness -- python3 /home/cg24/tgoskits/tools/starry-syscall-harness/mcp_server.py --repo /home/cg24/tgoskits
```

The MCP tools expose `doctor`, `discover`, `perf-profile`, and `perf-diff` flows.
