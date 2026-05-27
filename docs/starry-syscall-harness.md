# StarryOS Syscall Differential Harness

This harness compares small syscall probes on Linux and StarryOS, then reports semantic differences.

## Usage

Run from the repository root:

```bash
python3 tools/starry-syscall-harness/harness.py doctor
python3 tools/starry-syscall-harness/harness.py discover --arch riscv64
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

## Codex MCP

Register the MCP server locally:

```bash
codex mcp add starry-syscall-harness -- python3 /home/cg24/tgoskits/tools/starry-syscall-harness/mcp_server.py --repo /home/cg24/tgoskits
```

The MCP tools expose the same `doctor` and `discover` flows.
