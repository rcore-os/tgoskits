# OScope-harness

`OScope-harness` is packaged as a thin TGOSKits entrypoint. The harness source is not vendored in this repository; `prebuild.sh` clones `cg24-THU/tgoskit-harness_kit` at commit `b4fdf12c8479353d80e3d23960e653819db2a20d` and reuses that checkout from `target/tgoskit-harness-kit/`.

## CLI

From the TGOSKits repository root:

```bash
python3 apps/OScope-harness/harness.py doctor --no-docker
python3 apps/OScope-harness/harness.py discover --no-docker --arch riscv64
python3 apps/OScope-harness/harness.py perf-profile --no-docker --arch riscv64 --timeout 20 --format all
python3 apps/OScope-harness/harness.py ui --no-docker --host 127.0.0.1 --port 8765 --open
```

The wrapper keeps the original OScope command surface while the implementation stays in the fixed external harness kit commit.

## qperf Smoke

```bash
apps/OScope-harness/scripts/qperf-smoke.sh boot
apps/OScope-harness/scripts/qperf-smoke.sh blk-read
apps/OScope-harness/scripts/qperf-smoke.sh compare-self
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
