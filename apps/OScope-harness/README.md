# OScope-harness

`OScope-harness` is packaged as a thin TGOSKits entrypoint. The harness source is not vendored in this repository; `prebuild.sh` clones `cg24-THU/tgoskit-harness_kit` at commit `762c22725024a065e85b26e0b01121eccea651c0` and reuses that checkout from `target/tgoskit-harness-kit/`.

## Standalone CLI

From the TGOSKits repository root:

```bash
python3 apps/OScope-harness/harness.py doctor --no-docker
python3 apps/OScope-harness/harness.py discover --no-docker --arch riscv64
```

The wrapper keeps the original OScope command surface while the implementation stays in the fixed external harness kit commit.
The `doctor` and `discover` commands are the standalone smoke paths for this
app-packaging PR. They are the only paths documented here as independently
reproducible on this PR head.

## Runtime Companion Required

`perf-profile` and qperf smoke commands depend on the enhanced Starry qperf
runtime companion that extends `cargo xtask starry perf`. This PR only packages
the app wrappers; do not use these commands as standalone validation for this
PR. Apply the runtime companion or the combined runtime+app PR before expecting
profiling samples or smoke success:

```bash
python3 apps/OScope-harness/harness.py perf-profile --no-docker --arch riscv64 --timeout 20 --format all
apps/OScope-harness/scripts/qperf-smoke.sh boot
```

## MCP

Example MCP server configuration:

```json
{
  "mcpServers": {
    "OScope-harness": {
      "command": "python3",
      "args": [
        "/path/to/tgoskits/apps/OScope-harness/mcp_server.py",
        "--repo",
        "/path/to/tgoskits"
      ]
    }
  }
}
```
