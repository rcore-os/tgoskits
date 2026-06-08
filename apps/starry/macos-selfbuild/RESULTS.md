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
validated reproduction: SMP=8, JOBS=1
success marker: STARRY-MACOS-SELFBUILD-PASS
```

The validated path boots an 8-vCPU guest but intentionally runs one Cargo job.
It is a stable full self-build reproduction, not evidence that parallel guest
compilation with `JOBS=8` is supported.

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
  --features plat-dyn,ax-feat/defplat,ax-feat/ipi,ax-feat/irq,ax-feat/rtc,cntv-timer,smp \
  --release
```

with:

```text
CARGO_BUILD_JOBS=1
RAYON_NUM_THREADS=1
RUSTC_THREADS=1
CARGO_INCREMENTAL=0
CARGO_NET_OFFLINE=true
RUSTC_BOOTSTRAP=1
RUSTC=/opt/rustc-nightly-sysroot
RUSTDOC=/opt/rustdoc-nightly-sysroot
ALLOW_SLOW_SELFBUILD=0
```

The fast reproducible profile is guarded by the guest script. Unless
`ALLOW_SLOW_SELFBUILD=1` is set for experiments, it refuses the older
full-device profile containing `ax-feat/display`, `ax-driver/virtio-*`,
`starry-kernel/input`, or `starry-kernel/vsock`. That older profile expands to
about 386 crates and is the common reason for a run appearing to hang for more
than an hour.

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
| `SMP=8`, `JOBS=1`, `SOURCE_TMPFS=0`, tuned feature set | `657s` | latest validated default reproduction |
| `SMP=1`, `JOBS=1`, `SOURCE_TMPFS=0`, tuned feature set | `642s` | latest single-vCPU validation |
| `SMP=8`, `JOBS=1`, ext4 source/target | `951s` | slow guest baseline |
| `SMP=8`, `JOBS=8`, ext4 source/target | `917s` | historical experiment; not the current stable reproduction |
| `SMP=8`, `JOBS=8`, tmp target only | `660s` | historical experiment; not the current stable reproduction |
| `SMP=8`, `JOBS=8`, tmp source and target | `642s` | historical experiment; not the current stable reproduction |
| `SMP=8`, `JOBS=8`, tmp source/target, no LTO | `515s` | historical experiment; not the current stable reproduction |
| `SMP=8`, `JOBS=8`, tmp source/target, no LTO, opt0, CGU256 | `427s` | historical experiment; not the current stable reproduction |
| `SMP=8`, `JOBS=8`, tuned feature set, no LTO, opt0, CGU256, `RUSTC_THREADS=2` | `331s` | historical experiment; not the current stable reproduction |
| host macOS reference | `134s` | host-side lower bound, not inside StarryOS |

Useful ratios:

```text
951s / 657s = 1.45x   old slow guest baseline to latest stable reproduction
642s / 657s = 0.98x   single-vCPU and 8-vCPU single-job runs are effectively similar
```

## Interpretation

The macOS/HVF app demonstrates a real StarryOS guest self-build, not a host-side
cross build. The remaining gap to the macOS host reference is from filesystem
writeback, process/wait/pipe overhead, SMP scheduling and wakeups, lock
contention, and Cargo's serial critical path.
