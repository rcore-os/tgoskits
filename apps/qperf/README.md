# qperf

`qperf` is a QEMU TCG plugin and analyzer package for StarryOS kernel profiling. It samples guest PCs and frame-pointer call chains, resolves them against the kernel ELF, and exports folded stacks, SVG flamegraphs, pprof-compatible data, and qperf summary metrics.

This copy is packaged under `apps/qperf` so it can be used as the baseline profiling tool by `apps/OScope-harness`.

## Visual Preview

Baseline full-stack profile:

![qperf baseline full-stack flamegraph](../OScope-harness/assets/01_qperf_baseline_fullstack.png)

After a virtio-blk readahead experiment:

![qperf readahead full-stack flamegraph](../OScope-harness/assets/02_qperf_readahead_fullstack.png)

Focused virtio-blk hotspot view:

![qperf focused virtio-blk flamegraph](../OScope-harness/assets/03_qperf_blk_focus_flamegraph.png)

## Layout

- `src/`: QEMU plugin implementation.
- `analyzer/`: raw qperf sample decoder, symbol resolver, folded-stack exporter, diff command, and optional flamegraph renderer.
- `examples/`: synthetic demos for checking visualization behavior.
- `target/`: local build output; ignored by git.

## Requirements

- QEMU 9.2 or newer with TCG plugin API v4.
- Rust toolchain matching this repository.
- Kernel image with DWARF debug info and frame pointers.
- For StarryOS, `BACKTRACE=y` usually provides the required debug and frame-pointer settings.

## Build

From the repository root:

```bash
cargo build --manifest-path apps/qperf/Cargo.toml --release
cargo build --manifest-path apps/qperf/analyzer/Cargo.toml --release --features flamegraph
```

The plugin artifact is:

- Linux: `apps/qperf/target/release/libqperf.so`
- macOS: `apps/qperf/target/release/libqperf.dylib`

The analyzer artifact is `apps/qperf/target/release/qperf-analyzer`.

## Run

Attach the plugin to QEMU:

```bash
qemu-system-riscv64 ... -plugin apps/qperf/target/release/libqperf.so
```

Optional plugin parameters:

```bash
qemu-system-riscv64 ... \
  -plugin apps/qperf/target/release/libqperf.so,freq=101,out=target/qperf/qperf.bin,mode=tb,max_depth=96
```

Analyze raw samples:

```bash
apps/qperf/target/release/qperf-analyzer \
  --elf path/to/starryos.elf \
  target/qperf/qperf.bin \
  target/qperf/stack.folded
```

Generate a synthetic deep-stack flamegraph demo:

```bash
python3 apps/qperf/examples/long_chain_flamegraph_demo.py
```

## Integration

`apps/OScope-harness` consumes qperf artifacts and turns them into reports, hotspot tables, A/B diffs, UI views, and MCP tool results. Keep qperf source changes in `apps/qperf`; keep orchestration, reporting, and UI changes in `apps/OScope-harness`.
