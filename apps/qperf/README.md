# qperf

`qperf` is packaged as a thin TGOSKits entrypoint. The QEMU plugin and analyzer source is not vendored in this repository; `prebuild.sh` clones `cg24-THU/tgoskit-harness_kit` at commit `b4fdf12c8479353d80e3d23960e653819db2a20d` and reuses that checkout from `target/tgoskit-harness-kit/`.

Build the fixed qperf checkout:

```bash
apps/qperf/prebuild.sh cargo build --manifest-path tools/qperf/Cargo.toml --release
apps/qperf/prebuild.sh cargo build --manifest-path tools/qperf/analyzer/Cargo.toml --release --features flamegraph
```

Run the synthetic folded-stack demo:

```bash
apps/qperf/prebuild.sh python3 tools/qperf/examples/long_chain_flamegraph_demo.py --no-svg --output-dir target/qperf-long-chain-demo-smoke
```

Run `apps/qperf/prebuild.sh` without arguments to print the fixed checkout path.
