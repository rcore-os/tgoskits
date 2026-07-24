# qperf StarryOS Integration Report

## Summary

This change integrates the Starry-OS qperf profiler into the TGOSKits StarryOS
build flow. The main user-facing command is:

```bash
cargo starry perf --arch riscv64
```

The command now builds the bundled qperf plugin and analyzer, builds the
StarryOS kernel, starts QEMU with the qperf TCG plugin injected, runs the
analyzer, and writes a folded-stack report under `target/qperf`.

The riscv64 path was validated in Docker with image `b7c4600e825d`. The final
verification produced a non-empty folded stack containing StarryOS and ArceOS
kernel symbols such as `ax_plat`, `ax_task`, and `ax_mm`.

## Existing Code Structure

The `cargo starry` alias is defined in `.cargo/config.toml`:

```toml
starry = "run -p tg-xtask -- starry"
```

The StarryOS CLI entrypoint is in `scripts/axbuild/src/starry/mod.rs`.
Existing StarryOS build and rootfs helpers are in:

- `scripts/axbuild/src/starry/build.rs`
- `scripts/axbuild/src/starry/rootfs.rs`
- `scripts/axbuild/src/starry/mod.rs`

StarryOS QEMU arguments are loaded from the StarryOS QEMU templates and patched
by `rootfs::load_patched_qemu_config`, including managed rootfs selection,
network defaults, and SMP arguments. The existing `AppContext::qemu` path is
backed by `ostool`. For the qperf flow, that path cannot be used directly for
execution because debug mode causes QEMU to receive `-s -S`, which pauses the
guest before useful samples are produced. The qperf command therefore reuses the
existing build, rootfs, and QEMU config patching logic, writes the resulting
qperf QEMU config to the output directory, then starts QEMU directly with the
plugin-injected arguments.

The debug kernel artifacts are resolved from:

- ELF: `target/<target>/debug/starryos`
- raw image: `target/<target>/debug/starryos.bin`
- rootfs: `target/<target>/rootfs-<arch>.img`

For riscv64, the target is `riscv64gc-unknown-none-elf`.

## qperf Source Integration

qperf is bundled under `tools/qperf`. It contains:

- `tools/qperf/src/lib.rs`
- `tools/qperf/src/profiler.rs`
- `tools/qperf/src/reg.rs`
- `tools/qperf/analyzer/src/main.rs`

The plugin builds to:

```text
tools/qperf/target/release/libqperf.so
```

The analyzer builds to:

```text
tools/qperf/target/release/qperf-analyzer
```

The TGOSKits command builds both automatically, so users do not need to clone or
configure qperf manually.

## qperf Runtime Fixes

The original qperf plugin had several unbounded-growth and robustness risks:

- `crossbeam_channel::unbounded` allowed the writer queue to grow without a hard
  limit.
- Sampling used blocking send semantics on the QEMU execution path.
- Frame-pointer unwinding had no maximum stack depth.
- Frame-pointer chains had limited sanity checking.
- The writer used a raw file handle and could flush inefficiently.
- Sampling and writer paths could panic via `expect`.
- The analyzer had limited tolerance for truncated data and repeated address
  lookups.

The updated plugin now:

- uses a bounded channel, defaulting to 4096 entries in TGOSKits;
- uses `try_send` on the sampling path;
- drops samples when the queue is full and records `dropped_samples`;
- supports `max_depth` and defaults to 128 inside qperf;
- stops unwinding when frame pointers are zero, misaligned, repeated, not
  advancing, too deep, or unreadable;
- uses `BufWriter` for raw sample output;
- avoids panics in sampling and writing paths, recording `sample_failures`
  instead;
- emits a plugin summary on normal shutdown with format version, sample counts,
  dropped sample count, frequency, stack depth, architecture, and output path.

The analyzer now:

- uses buffered input/output;
- caches symbol resolution by address;
- tolerates trailing partial records after at least one valid record;
- maps empty or unresolved stacks to `??` or hexadecimal addresses instead of
  aborting the whole run;
- returns contextual errors for file and symbol-loader failures.

When QEMU is stopped by an external timeout, the plugin may not run its normal
shutdown path. In that case the TGOSKits integration still writes
`summary.txt`, but marks plugin-side dropped sample counts as unknown.

## TGOSKits Command

The new subcommand is defined in `scripts/axbuild/src/starry/mod.rs`:

```bash
cargo starry perf [OPTIONS]
```

Supported options:

- `--arch <ARCH>`: `riscv64` or `loongarch64`
- `--freq <N>`: sampling frequency, default `99`
- `--out <DIR>`: output directory
- `--format <folded|svg|pprof|all>`: default `all`
- `--max-depth <N>`: maximum unwind depth, default `64`
- `--timeout <SECONDS>`: QEMU runtime limit, default `20`

`pprof` is reserved and currently returns a clear unsupported-format error.

Default output layout:

```text
target/qperf/<arch>/<timestamp>/
  qemu.toml
  qperf.bin
  stack.folded
  flamegraph.svg        # optional, when a generator is installed
  summary.txt
```

For explicit output directories:

```bash
cargo starry perf --arch riscv64 --out target/qperf/my-run
```

## Flamegraph Generation

`--format all` and `--format svg` attempt to find one of:

- `inferno-flamegraph`
- `flamegraph`
- `flamegraph.pl`

If none is present, the command does not fail. It keeps `stack.folded` and prints
a message explaining that a flamegraph generator is missing.

To reproduce SVG generation:

```bash
cargo install inferno
cargo starry perf --arch riscv64 --format all
```

Or manually:

```bash
inferno-flamegraph < target/qperf/.../stack.folded > target/qperf/.../flamegraph.svg
```

## Manual qperf Validation

Before integration, qperf was manually validated against StarryOS in Docker.

Build qperf plugin and analyzer:

```bash
docker run --rm \
  -v "$PWD":/work \
  -v /tmp/qperf:/qperf \
  -w /qperf \
  b7c4600e825d \
  bash -lc 'cargo build --release && cargo build --release -p qperf-analyzer'
```

Build StarryOS and prepare rootfs:

```bash
docker run --rm \
  -v "$PWD":/work \
  -v /tmp/qperf:/qperf \
  -w /work \
  b7c4600e825d \
  bash -lc 'cargo starry build --arch riscv64 --debug && cargo starry rootfs --arch riscv64'
```

Run QEMU manually with qperf:

```bash
docker run --rm \
  -v "$PWD":/work \
  -v /tmp/qperf:/qperf \
  -w /work \
  b7c4600e825d \
  bash -lc 'mkdir -p target/qperf-manual-riscv64 && timeout 20s qemu-system-riscv64 -nographic -cpu rv64 -machine virt -kernel target/riscv64gc-unknown-none-elf/debug/starryos.bin -device virtio-blk-pci,drive=disk0 -drive id=disk0,if=none,format=raw,file=target/riscv64gc-unknown-none-elf/rootfs-riscv64.img -device virtio-net-pci,netdev=net0 -netdev user,id=net0 -plugin /qperf/target/release/libqperf.so,freq=99,out=target/qperf-manual-riscv64/qperf.bin || true'
```

Analyze samples:

```bash
docker run --rm \
  -v "$PWD":/work \
  -v /tmp/qperf:/qperf \
  -w /work \
  b7c4600e825d \
  bash -lc '/qperf/target/release/qperf-analyzer -e target/riscv64gc-unknown-none-elf/debug/starryos target/qperf-manual-riscv64/qperf.bin target/qperf-manual-riscv64/stack.folded'
```

This produced a non-empty `stack.folded` with kernel symbols.

## Integrated Verification

All verification was run in Docker image `b7c4600e825d`.

Format check:

```bash
docker run --rm -v "$PWD":/work -w /work b7c4600e825d \
  bash -lc 'cargo fmt --check'
```

Result: passed.

Targeted clippy for the modified TGOSKits crate:

```bash
docker run --rm -v "$PWD":/work -w /work b7c4600e825d \
  bash -lc 'cargo xtask clippy --package axbuild'
```

Result: passed, 4 checks passed.

Targeted tests:

```bash
docker run --rm -v "$PWD":/work -w /work b7c4600e825d \
  bash -lc 'cargo test -p axbuild'
```

Result: passed, 207 tests passed.

qperf plugin and analyzer clippy:

```bash
docker run --rm -v "$PWD":/work -w /work/tools/qperf b7c4600e825d \
  bash -lc 'cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo clippy --workspace --all-targets -- -D warnings'
```

Result: passed.

Help output:

```bash
docker run --rm -v "$PWD":/work -w /work b7c4600e825d \
  bash -lc 'cargo starry perf --help'
```

Result: passed. The command shows `--arch`, `--freq`, `--out`, `--format`,
`--max-depth`, and `--timeout`.

Final riscv64 integrated run:

```bash
docker run --rm -v "$PWD":/work -w /work b7c4600e825d \
  bash -lc 'cargo starry perf --arch riscv64 --timeout 20 --format folded --out target/qperf/integration-riscv64-final'
```

Result: passed. QEMU was stopped by `timeout` after samples had been produced,
and the analyzer generated:

```text
target/qperf/integration-riscv64-final/qperf.bin
target/qperf/integration-riscv64-final/stack.folded
target/qperf/integration-riscv64-final/summary.txt
```

`stack.folded` contained 1630 lines. Symbol checks found StarryOS and ArceOS
kernel symbols including:

- `ax_plat::call_main`
- `ax_task::api::current_may_uninit`
- `ax_mm::backend::Backend`
- page-table mapping functions

`--format all` was also verified with a 20 second timeout. The container did
not contain a flamegraph generator, so no SVG was produced, but the command
completed successfully and retained the folded stacks.

## Known Limitations

- `cargo starry qemu --perf` is not implemented. The current scope keeps the
  integration as a dedicated `cargo starry perf` subcommand to avoid changing
  existing `qemu` behavior.
- `--format pprof` is reserved but not implemented.
- loongarch64 support is wired at the argument/QEMU selection level, but it was
  not validated in this environment.
- The plugin shutdown summary is unavailable when QEMU is terminated by
  external timeout before plugin shutdown. The integration-level `summary.txt`
  still records paths and folded stack line counts.
- Full workspace `cargo test` was not run due workspace size; targeted
  `axbuild` tests and qperf checks were run instead.

## Reproduction

Run:

```bash
docker run --rm -v "$PWD":/work -w /work b7c4600e825d \
  bash -lc 'cargo starry perf --arch riscv64 --timeout 20 --format folded --out target/qperf/repro-riscv64'
```

Then inspect:

```bash
wc -l target/qperf/repro-riscv64/stack.folded
grep -E 'ax_task|ax_mm|ax_plat|syscall|starry' target/qperf/repro-riscv64/stack.folded | head
cat target/qperf/repro-riscv64/summary.txt
```
