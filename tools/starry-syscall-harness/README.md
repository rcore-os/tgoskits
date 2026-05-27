# StarryOS Syscall Harness

This harness compares selected StarryOS syscall behavior with Linux, profiles StarryOS qperf hotspots, and emits reports that can guide focused fixes.

All StarryOS build, QEMU, syscall probe, and qperf runs should stay inside the configured Docker image.

## CLI

```bash
python3 tools/starry-syscall-harness/harness.py doctor
python3 tools/starry-syscall-harness/harness.py discover --arch riscv64
python3 tools/starry-syscall-harness/harness.py perf-profile --arch riscv64 --timeout 20 --format all
python3 tools/starry-syscall-harness/harness.py perf-diff --baseline target/starry-syscall-harness/perf/riscv64/latest --compare target/starry-syscall-harness/perf/riscv64/latest
```

Reports are written under `target/starry-syscall-harness`.

## Local UI

The browser UI is optional and uses the same harness commands behind a local API:

```bash
python3 tools/starry-syscall-harness/harness.py ui --host 127.0.0.1 --port 8765 --open
```

The UI can start syscall scans, qperf profiling, perf diffs, and Doctor checks. It reads JSON reports and qperf flamegraphs from `target/starry-syscall-harness`.

## MCP

`mcp_server.py` exposes the CLI workflows as MCP tools:

- `starry_syscall_doctor`
- `starry_syscall_discover`
- `starry_perf_profile`
- `starry_perf_diff`
- `starry_harness_ui_command`
