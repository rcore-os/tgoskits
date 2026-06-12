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
QEMU disk mode: writable working-copy image; optional QEMU_SNAPSHOT=1 overlay
validated reproduction: SMP=8, JOBS=8
success marker: STARRY-MACOS-SELFBUILD-PASS
```

The validated path boots an 8-vCPU guest and runs Cargo with eight jobs.

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
  -Z build-std=core,alloc \
  --target-dir /tmp/starryos-selfbuild-target \
  --features plat-dyn,ax-driver/virtio-blk,ax-driver/virtio-net,ax-driver/virtio-gpu,ax-driver/virtio-input,ax-driver/virtio-socket,starry-kernel/input,starry-kernel/vsock \
  --release
```

with:

```text
CARGO_BUILD_JOBS=8
RAYON_NUM_THREADS=1
RUSTC_THREADS=2
CARGO_INCREMENTAL=0
CARGO_NET_OFFLINE=true
RUSTC_BOOTSTRAP=1
RUSTC=/opt/rustc-nightly-sysroot
RUSTDOC=/opt/rustdoc-nightly-sysroot
ALLOW_SLOW_SELFBUILD=0
```

The qemu-aarch64 reproducible profile is guarded by the guest script. Unless
`ALLOW_SLOW_SELFBUILD=1` is set for experiments, it refuses runs that do not use
`RUSTC_THREADS=2`.

## Reference Numbers

These are local Apple Silicon reference measurements from the macOS/HVF
self-build experiments. Re-run locally for authoritative numbers on another
machine.

| Case | Time | Notes |
| --- | --- | --- |
| `SMP=8`, `JOBS=8`, qemu-aarch64 profile, tmp source/target, `RUSTC_THREADS=2` | `331s` | latest validated default reproduction |
| `SMP=8`, `JOBS=1`, `SOURCE_TMPFS=0`, tuned feature set | `657s` | historical single-job fallback |
| `SMP=1`, `JOBS=1`, `SOURCE_TMPFS=0`, tuned feature set | `642s` | latest single-vCPU validation |
| `SMP=8`, `JOBS=1`, ext4 source/target | `951s` | slow guest baseline |
| `SMP=8`, `JOBS=8`, ext4 source/target | `917s` | first complete SMP self-build |
| `SMP=8`, `JOBS=8`, tmp target only | `660s` | moves Cargo target output to `/tmp` |
| `SMP=8`, `JOBS=8`, tmp source and target | `642s` | copies source and target output to `/tmp` |
| `SMP=8`, `JOBS=8`, tmp source/target, no LTO | `515s` | `CARGO_PROFILE_RELEASE_LTO=false` |
| `SMP=8`, `JOBS=8`, tmp source/target, no LTO, opt0, CGU256 | `427s` | reduces serial optimized codegen cost |
| host macOS reference | `134s` | host-side lower bound, not inside StarryOS |

Useful ratios:

```text
951s / 331s = 2.87x   slow guest baseline to latest stable reproduction
642s / 331s = 1.94x   tmp source/target baseline to latest stable reproduction
657s / 331s = 1.99x   single-job fallback to eight-job default reproduction
```

## Interpretation

The macOS/HVF app demonstrates a real StarryOS guest self-build, not a host-side
cross build. The remaining gap to the macOS host reference is from filesystem
writeback, process/wait/pipe overhead, SMP scheduling and wakeups, lock
contention, and Cargo's serial critical path.
