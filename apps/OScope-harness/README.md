# OScope-harness

`OScope-harness` is packaged as a thin TGOSKits entrypoint. The harness source is not vendored in this repository; `prebuild.sh` clones `cg24-THU/tgoskit-harness_kit` at commit `762c22725024a065e85b26e0b01121eccea651c0` and reuses that checkout from `target/tgoskit-harness-kit/`.

## CLI

From the TGOSKits repository root:

```bash
python3 apps/OScope-harness/harness.py doctor --no-docker
python3 apps/OScope-harness/harness.py discover --no-docker --arch riscv64
python3 apps/OScope-harness/harness.py perf-profile --no-docker --arch riscv64 --timeout 20 --format all
python3 apps/OScope-harness/harness.py ui --no-docker --host 127.0.0.1 --port 8765 --open
```

The wrapper keeps the original OScope command surface while the implementation stays in the fixed external harness kit commit.
This package also wires the Starry qperf runtime into `cargo xtask starry perf`
so profiling commands can run through the app wrapper.

## qperf Smoke

```bash
apps/OScope-harness/scripts/qperf-smoke.sh boot
```

qperf smoke commands use the enhanced Starry runtime added here, including
`--case`, marker, and shell-init support:

```bash
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
