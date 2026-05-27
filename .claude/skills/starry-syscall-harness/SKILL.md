---
name: starry-syscall-harness
description: Audit and fix StarryOS syscall Linux-compatibility issues using the project syscall differential harness. Use when comparing StarryOS syscall behavior with Linux, running or extending tools/starry-syscall-harness, invoking its MCP tools, or validating syscall fixes before opening a PR.
---

# Starry Syscall Harness

Use this skill for StarryOS syscall semantic audits and fixes.

## Core Rules

- Run all StarryOS build, rootfs, QEMU, and syscall probe execution through Docker.
- Use the configured image by default: `ghcr.io/rcore-os/tgoskits-container:latest`.
- Prefer the harness entrypoint over hand-built commands:
  `python3 tools/starry-syscall-harness/harness.py discover --arch riscv64`
- Treat host Linux probe output as the reference unless a case is explicitly documented as architecture-specific.
- Keep fixes in the relevant syscall implementation; do not weaken probes to hide mismatches.
- After changing StarryOS logic, run `cargo fmt` and targeted clippy in Docker:
  `docker run --rm -v "$PWD":/work -w /work ghcr.io/rcore-os/tgoskits-container:latest bash -lc 'cargo xtask clippy --package starry-kernel'`

## Workflow

1. Run `python3 tools/starry-syscall-harness/harness.py doctor` to verify Docker and required tools.
2. Run `python3 tools/starry-syscall-harness/harness.py discover --arch riscv64`.
3. Read the JSON report under `target/starry-syscall-harness/<arch>/latest/report.json`.
4. Fix only mismatches with clear Linux semantics.
5. Rerun the harness for the affected arch, then run Docker clippy for changed crates.
6. Before PR work, fetch upstream and create a clean branch from the target upstream branch.

## MCP

The MCP server is `tools/starry-syscall-harness/mcp_server.py`.

Register it locally with:

```bash
codex mcp add starry-syscall-harness -- python3 /home/cg24/tgoskits/tools/starry-syscall-harness/mcp_server.py --repo /home/cg24/tgoskits
```

Available tools:

- `starry_syscall_doctor`: checks Docker, image, and required toolchain availability.
- `starry_syscall_discover`: runs Linux-vs-StarryOS syscall probes and returns the report.

## Probe Changes

When adding a probe:

- Put deterministic C cases in `tools/starry-syscall-harness/probes/syscall_probe.c`.
- Print one `CASE <name> key=value ...` line per syscall behavior.
- Avoid fd numbers, timestamps, paths, pointer values, and scheduler-sensitive output in comparisons.
- Prefer small syscall-focused probes that can run under BusyBox init without extra packages.
