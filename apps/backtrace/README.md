# Backtrace Showcase

This directory is a runnable guide for the TGOSKits backtrace demos. It is
intended for contributors and reviewers: start here when you need to configure
the environment, build StarryOS/ArceOS, run the demos, enable backtrace support
in another program, and check what each backtrace mode proves.

The demos cover four paths:

| Demo | Command target                               | What it shows                                                     |
| ---- | -------------------------------------------- | ----------------------------------------------------------------- |
| 1    | ArceOS `backtrace` with `--no-symbolize`     | Target-side raw `BACKTRACE_BEGIN` / `BT` / `BACKTRACE_END` output |
| 2    | ArceOS `backtrace` with auto symbolize       | Host-side auto-symbolization from the same build's ELF            |
| 3    | ArceOS `backtrace-raw-normal` with `DWARF=y` | Deeper frame-pointer chain plus DWARF-enabled host symbolize      |
| 4    | StarryOS `qemu/memtrack-backtrace` app       | Allocation tracking raw backtraces plus host symbolize            |

For expected log markers, see [EXPECTED.md](EXPECTED.md).

## Architecture Coverage

The showcase is configured for all four TGOSKits QEMU architectures:

| Demo | x86_64 | aarch64 | riscv64 | loongarch64 |
| ---- | ------ | ------- | ------- | ----------- |
| 1: ArceOS raw, no symbolize | yes | yes | yes | yes |
| 2: ArceOS auto host symbolize | yes | yes | yes | yes |
| 3: ArceOS DWARF raw-normal | yes | yes | yes | yes |
| 4: StarryOS memtrack backtrace | yes | yes | yes | yes |

Architecture names map to these Rust target triples:

| Arch | Target triple |
| ---- | ------------- |
| `x86_64` | `x86_64-unknown-none` |
| `aarch64` | `aarch64-unknown-none-softfloat` |
| `riscv64` | `riscv64gc-unknown-none-elf` |
| `loongarch64` | `loongarch64-unknown-none-softfloat` |

Use any supported architecture as the optional second argument:

```bash
bash apps/backtrace/run_demo.sh demo3 riscv64
bash apps/backtrace/run_demo.sh demo4 aarch64
```

Run one demo across the whole matrix:

```bash
bash apps/backtrace/run_demo.sh demo3-all
bash apps/backtrace/run_demo.sh demo4-all
```

Run the complete matrix:

```bash
bash apps/backtrace/run_demo.sh all-arch
```

Coverage means each demo has a per-architecture build config, QEMU config, and
helper-script entry point. In local Docker validation, Demo 3 prints symbolized
`c -> b -> a -> main` frame chains on all four architectures. Demo 4 emits
allocation raw blocks and the helper script host-symbolizes `kind=alloc` blocks
on all four architectures.

## Use Backtrace In Your Own Program

There are three pieces to enable backtrace in another ArceOS or StarryOS
program: dependency/features, a raw-block print point, and build-time flags.

For an ArceOS Rust app that wants to print an explicit raw block, add
`axbacktrace` and keep the usual `ax-std` feature path enabled:

```toml
[features]
ax-std = ["dep:ax-std"]

[dependencies]
ax-std = { workspace = true, optional = true, features = ["backtrace"] }
axbacktrace = { workspace = true }
```

Print a machine-readable raw block from the place you want to inspect:

```rust
use axbacktrace::Backtrace;

fn show_backtrace() {
    println!("{}", Backtrace::capture().kind("raw"));
}
```

For a trap/exception path, use the architecture trap context helper when one is
available, or call `Backtrace::capture_trap(fp, ip, ra)` only when the caller
already has a trustworthy frame pointer, instruction pointer, and return
address.

In the app's `build-<target>.toml`, enable one of these environment flags:

```toml
[env]
BACKTRACE = "y"
```

`BACKTRACE=y` tells axbuild to keep frame pointers, which is required for the
target-side frame-pointer unwind. If you also need function/file/line
symbolization from the host, use `DWARF=y`:

```toml
[env]
DWARF = "y"
```

`DWARF=y` implies backtrace support, keeps frame pointers, and preserves debug
information in the ELF. ArceOS QEMU tests with `BACKTRACE=y` or `DWARF=y`
automatically run the host symbolizer after QEMU unless `--no-symbolize` is
passed.

For StarryOS allocation tracking, enable the memtrack feature and backtrace
build flag in the StarryOS app config:

```toml
env = { BACKTRACE = "y" }
features = [
  "starry-kernel/memtrack",
]
```

Then trigger `/dev/memtrack` from the guest shell. Demo 4 writes `start`,
`sample`, `sample_hard`, `symbolize`, and `end` to show allocation backtrace
capture and reporting.

## Prerequisites

Run commands from the repository root.

Recommended environment:

- Rust toolchain configured for this repository.
- QEMU for the architecture you run, such as `qemu-system-x86_64`,
  `qemu-system-aarch64`, `qemu-system-riscv64`, or
  `qemu-system-loongarch64`.
- `llvm-addr2line` or `addr2line` in `PATH`.
- For StarryOS rootfs manipulation, the tools documented by the TGOSKits
  quick-start/container environment.

The project container is the most reproducible setup:

```bash
docker pull ghcr.io/rcore-os/tgoskits-container:latest
docker run -it --rm \
  -v "$(pwd)":/workspace \
  -w /workspace \
  ghcr.io/rcore-os/tgoskits-container:latest
```

If you are running on the host directly, first check:

```bash
rustc --version
cargo --version
qemu-system-x86_64 --version
llvm-addr2line --version || addr2line --version
```

## Prepare StarryOS

The ArceOS demos build self-contained QEMU test images. The StarryOS memtrack
demo uses a managed Alpine rootfs. `starry app qemu` can prepare it on demand,
but this command is useful when you want to download/patch the rootfs before
running Demo 4:

```bash
bash apps/backtrace/run_demo.sh starry-rootfs-all
```

Optional smoke run:

```bash
cargo xtask starry qemu --arch x86_64
```

## Run All Demos

The helper script wraps the canonical commands below and keeps the workflow easy
to reproduce in a PR discussion. Demo 4 additionally captures the StarryOS app
log and runs the host symbolizer for the captured allocation blocks.

```bash
bash apps/backtrace/run_demo.sh all x86_64
```

You can also run one demo at a time:

```bash
bash apps/backtrace/run_demo.sh demo1 x86_64
bash apps/backtrace/run_demo.sh demo2 x86_64
bash apps/backtrace/run_demo.sh demo3 x86_64
bash apps/backtrace/run_demo.sh demo4 x86_64
```

Omit the architecture argument to use `x86_64`.

## Demo 1: Raw Backtrace, No Host Symbolize

```bash
cargo xtask arceos test qemu \
  --arch <arch> \
  --test-group rust \
  --test-case backtrace \
  --no-symbolize
```

Expected result:

- QEMU prints a raw block with `BACKTRACE_BEGIN`.
- The block contains at least `BT 0` and `BT 1`.
- The test ends with `test pass`.
- No `=== host backtrace symbolize ===` section is expected because
  `--no-symbolize` disables host symbolization.

## Demo 2: Auto Host Symbolize

```bash
cargo xtask arceos test qemu \
  --arch <arch> \
  --test-group rust \
  --test-case backtrace
```

Expected result:

- The target still emits the raw backtrace block.
- The host prints `=== host backtrace symbolize ===` after QEMU.
- The symbolized block contains function names resolved from the same build's
  ELF.

## Demo 3: DWARF Auto Symbolize

```bash
cargo xtask arceos test qemu \
  --arch <arch> \
  --test-group rust \
  --test-case backtrace-raw-normal
```

Expected result:

- The target prints `emitting raw backtrace report (normal fp chain)...`.
- The raw block uses `kind=raw`.
- The host symbolizer runs automatically because the build config enables
  `DWARF=y`.
- The symbolized output should include the synthetic call chain from the test
  program, such as `c`, `b`, and `a`, when debug information is available.

## Demo 4: StarryOS Memtrack Backtrace

```bash
bash apps/backtrace/run_demo.sh demo4 <arch>
```

The helper script runs the StarryOS QEMU app, keeps the QEMU transcript in
`/tmp`, then host-symbolizes allocation raw blocks against
`target/<target-triple>/release/starryos`.

The underlying app command is:

```bash
cargo xtask starry app qemu \
  -t qemu/memtrack-backtrace \
  --arch <arch> \
  --qemu-config qemu-<arch>.toml
```

Expected result:

- StarryOS boots with `starry-kernel/memtrack` enabled.
- The QEMU shell command sequence writes to `/dev/memtrack`:
  `start`, `sample`, `sample_hard`, `symbolize`, and `end`.
- The guest prints:
  - `Memory allocation sample recorded`
  - `Hard memory allocation sample recorded`
  - at least one `BACKTRACE_BEGIN kind=alloc` raw block
  - `STARRY_MEMTRACK_BACKTRACE_OK`
- The helper prints `=== host backtrace symbolize ===` after QEMU.
- The symbolized output contains `BACKTRACE_BLOCK ... kind=alloc`; when symbols
  are available, it includes frames such as `starry_memtrack_symbolize_probe`.

Demo 4 uses `--adjust-ip false` when host-symbolizing StarryOS memtrack
allocation blocks. The default host symbolizer adjustment is useful for normal
frame-pointer backtraces whose IPs are return addresses. Memtrack allocation
blocks record allocation/probe sample addresses that should be symbolized as
recorded; on aarch64, subtracting the default 4-byte call-site adjustment can
move the probe frame away from `starry_memtrack_symbolize_probe`.

The raw `cargo xtask starry app qemu` command validates the guest markers. Use
the helper, or the manual command below, when you also want host symbolization
for the same allocation blocks.

## Manual Host Symbolize

If you keep or capture a QEMU log manually, you can symbolize it afterwards:

```bash
cargo xtask backtrace symbolize \
  --elf target/<target-triple>/release/arceos-backtrace-raw-normal \
  --log /tmp/arceos-backtrace.log \
  --kind raw
```

For the StarryOS memtrack app, capture the app output and symbolize `kind=alloc`
blocks against the StarryOS ELF. Keep `--adjust-ip false` for memtrack logs so
allocation/probe sample IPs are resolved at the recorded addresses:

```bash
cargo xtask starry app qemu \
  -t qemu/memtrack-backtrace \
  --arch <arch> \
  --qemu-config qemu-<arch>.toml \
  2>&1 | tee /tmp/starry-memtrack-backtrace.log

cargo xtask backtrace symbolize \
  --elf target/<target-triple>/release/starryos \
  --log /tmp/starry-memtrack-backtrace.log \
  --kind alloc \
  --adjust-ip false
```

For automatic QEMU log retention, set:

```bash
TGOSKITS_KEEP_QEMU_LOG=1 cargo xtask arceos test qemu \
  --arch x86_64 \
  --test-group rust \
  --test-case backtrace-raw-normal
```

## Troubleshooting

- If no host-symbolized section appears, confirm the build config enables
  `BACKTRACE=y` or `DWARF=y`, and that `llvm-addr2line` or `addr2line` is in
  `PATH`.
- If Demo 4 does not symbolize, check the raw log path printed by the helper and
  confirm `target/<target-triple>/release/starryos` exists for the selected
  architecture.
- If the StarryOS demo fails before booting the shell, regenerate the rootfs:
  `bash apps/backtrace/run_demo.sh starry-rootfs <arch>`.
- If frame chains are shallow, confirm the build keeps frame pointers. These
  demos rely on the build flags inserted by TGOSKits when backtrace support is
  enabled.
- If a StarryOS app appears to pause after build at `starry-kallsyms.sh`, wait
  for the existing kallsyms padding step to finish; it can take a few minutes
  on large release ELFs before QEMU starts.
- If behavior differs across machines, rerun inside
  `ghcr.io/rcore-os/tgoskits-container:latest`.
