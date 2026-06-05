# OScope-harness

`OScope-harness` is packaged as a thin TGOSKits entrypoint. The harness source is not vendored in this repository; `prebuild.sh` clones `cg24-THU/tgoskit-harness_kit` at commit `762c22725024a065e85b26e0b01121eccea651c0` and reuses that checkout from `target/tgoskit-harness-kit/`.

## CLI

From the TGOSKits repository root:

```bash
python3 apps/OScope-harness/harness.py doctor --no-docker
python3 apps/OScope-harness/harness.py discover --no-docker --arch riscv64
python3 apps/OScope-harness/harness.py ui --no-docker --host 127.0.0.1 --port 8765 --open
```

The wrapper keeps the original OScope command surface while the implementation stays in the fixed external harness kit commit.
The `doctor` and `discover` commands are the standalone smoke paths for this
app-packaging PR. Performance profiling requires the Starry qperf runtime
companion that extends `cargo xtask starry perf`.

## qperf Smoke

```bash
apps/OScope-harness/scripts/qperf-smoke.sh boot
```

qperf smoke commands require the enhanced Starry runtime companion that adds
`--case`, marker, and shell-init support to `cargo xtask starry perf`. This PR
only packages the app wrappers; apply the runtime companion or the combined
runtime+app PR before using this smoke entrypoint.

After the runtime companion is present, profiling can be launched with:

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
