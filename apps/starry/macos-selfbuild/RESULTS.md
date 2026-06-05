# macOS HVF Self-Build Result Notes

This file records the expected result shape for the macOS/HVF self-build
workflow. The authoritative reproduction command is in `README.md`.

## Control Variables

```text
host OS: macOS on Apple Silicon
guest ISA: AArch64
accelerator: QEMU HVF
kernel: StarryOS AArch64 SMP
rootfs: macOS-native generated self-build ext4 image
QEMU disk mode: -snapshot
success marker: STARRY-MACOS-SELFBUILD-PASS
```

The git repository contains the runner, checks, source-level fixes, and the
macOS-native rootfs construction script. The generated rootfs is still kept out
of git because it is large, but reviewers should not need Docker or a prebuilt
artifact to reproduce the macOS/HVF run. A prebuilt rootfs can be supplied only
as a faster optional path.

## Guest Build Shape

Inside the StarryOS guest, the runner executes:

```bash
cargo build \
  -p starryos \
  --bin starryos \
  --target aarch64-unknown-none-softfloat \
  -Z build-std=core,alloc,compiler_builtins \
  --target-dir /tmp/starryos-selfbuild-target \
  --features ax-feat/defplat,ax-feat/irq,ax-feat/ipi,ax-feat/rtc,cntv-timer,smp \
  --release
```

with:

```text
CARGO_BUILD_JOBS=<JOBS>
RAYON_NUM_THREADS=1
RUSTC_THREADS=2
CARGO_INCREMENTAL=0
CARGO_NET_OFFLINE=true
RUSTC_BOOTSTRAP=1
RUSTC=/opt/rustc-nightly-sysroot
RUSTDOC=/opt/rustdoc-nightly-sysroot
```

The guest source copy also patches `lwprintf-rs` to the local
`apps/starry/macos-selfbuild/crates/lwprintf-rs` compatibility crate. This keeps
the self-build path from requiring guest `dlopen`, because upstream
`lwprintf-rs` runs bindgen and dynamically loads libclang in its build script.

## Reference Numbers

These are local Apple Silicon reference measurements from the macOS/HVF
self-build experiments. Re-run locally for authoritative numbers on another
machine.

| Case | Time | Notes |
| --- | --- | --- |
| `SMP=8`, `JOBS=1`, ext4 source/target | `951s` | slow guest baseline |
| `SMP=8`, `JOBS=8`, ext4 source/target | `917s` | first complete SMP self-build |
| `SMP=8`, `JOBS=8`, tmp target only | `660s` | moves Cargo target output to `/tmp` |
| `SMP=8`, `JOBS=8`, tmp source and target | `642s` | copies source and target output to `/tmp` |
| `SMP=8`, `JOBS=8`, tmp source/target, no LTO | `515s` | `CARGO_PROFILE_RELEASE_LTO=false` |
| `SMP=8`, `JOBS=8`, tmp source/target, no LTO, opt0, CGU256 | `427s` | reduces serial optimized codegen cost |
| `SMP=8`, `JOBS=8`, tuned feature set, no LTO, opt0, CGU256, `RUSTC_THREADS=2` | `331s` | best local full self-build |
| host macOS reference | `134s` | host-side lower bound, not inside StarryOS |

Useful ratios:

```text
951s / 331s = 2.87x   slow guest baseline to tuned local best
642s / 331s = 1.94x   tmp source/target baseline to tuned local best
422s / 331s = 1.28x   tuned JOBS=1 to tuned JOBS=8
```

## Interpretation

The macOS/HVF app demonstrates a real StarryOS guest self-build, not a host-side
cross build. The remaining gap to the macOS host reference is from filesystem
writeback, process/wait/pipe overhead, SMP scheduling and wakeups, lock
contention, and Cargo's serial critical path.
