# macOS HVF Self-Build Result Notes

This file records the stable result shape expected from the macOS/HVF
self-build workflow.

## Control Variables

```text
host OS: macOS on Apple Silicon
guest ISA: AArch64
accelerator: QEMU HVF
kernel: StarryOS AArch64 SMP
rootfs: prepared ext4 image with Cargo/Rust and /opt/tgoskits
QEMU disk mode: -snapshot
success marker: STARRY-MACOS-SELFBUILD-PASS
```

The committed app does not include the large rootfs, toolchain, kernel image, or
local logs. A reproducible run starts from a prepared rootfs that passes
`check_rootfs.sh`.

## Current Validation After Dev Sync

The reproducibility path was rechecked from a separate local rootfs environment:

```text
prepare_rootfs.sh from rootfs-aarch64-hvf-toolchain.img
  -> STARRY_MACOS_SELFBUILD_ROOTFS_OK

build_kernel.sh
  -> target/aarch64-unknown-none-softfloat/release/starryos.bin

BOOT_ONLY=1 QEMU_ACCEL='tcg,thread=multi' QEMU_CPU=cortex-a53 SMP=1
  -> StarryOS shell prompt observed, runner exits with rc=0
```

On the same local host, QEMU 11.0.1 with HVF currently aborts before the guest
shell:

```text
Assertion failed: (isv), function hvf_handle_exception, file hvf.c
===HOST-QEMU-STOP reason=qemu-exit ... rc=134===
```

That failure happens with both `SMP=8` and `SMP=1`, so it is not a rootfs
construction failure and not only an SMP scheduling issue. The runner keeps HVF
as the default fast path, but exposes `QEMU_ACCEL`, `QEMU_MACHINE`, `QEMU_CPU`,
and `BOOT_ONLY` so the rootfs/kernel setup can be reproduced separately from a
local QEMU/HVF decoder crash.

## Build Command Shape

Inside the StarryOS guest, the app runs:

```bash
cargo build \
  -p starryos \
  --bin starryos \
  --target aarch64-unknown-none-softfloat \
  -Z build-std=core,alloc,compiler_builtins \
  --target-dir /tmp/starryos-selfbuild-target \
  --features plat-dyn,cntv-timer,smp,ax-feat/display,ax-feat/rtc,ax-driver/virtio-blk,ax-driver/virtio-net,ax-driver/virtio-gpu,ax-driver/virtio-input,ax-driver/virtio-socket,starry-kernel/input,starry-kernel/vsock \
  --release
```

with:

```text
CARGO_BUILD_JOBS=<JOBS>
RAYON_NUM_THREADS=<JOBS>
CARGO_INCREMENTAL=0
CARGO_NET_OFFLINE=true
RUSTC_BOOTSTRAP=1
RUSTC=/opt/rustc-nightly-sysroot
RUSTDOC=/opt/rustdoc-nightly-sysroot
```

The default runner uses the build shape above. Local tuning runs may override
`FEATURES`, release-profile settings, or `EXTRA_RUSTFLAGS`; those overrides must
be recorded beside the number.

## Reference Numbers

| Case | Time | Notes |
| --- | --- | --- |
| `SMP=8`, `JOBS=1`, ext4 source/target | `951s` | slow guest baseline |
| `SMP=8`, `JOBS=8`, ext4 source/target | `917s` | first complete SMP self-build |
| `SMP=8`, `JOBS=8`, tmp target only | `660s` | moves Cargo target output to `/tmp` |
| `SMP=8`, `JOBS=8`, tmp source and target | `642s` | copies source and target output to `/tmp` |
| `SMP=8`, `JOBS=8`, tmp source/target, no LTO | `515s` | `CARGO_PROFILE_RELEASE_LTO=false` |
| `SMP=8`, `JOBS=8`, tmp source/target, no LTO, opt0, CGU256 | `427s` | reduces serial optimized codegen cost |
| `SMP=8`, `JOBS=8`, tuned feature set, no LTO, opt0, CGU256 | `331s` | best local full self-build |
| host macOS reference | `134s` | separate host-side lower bound, not inside StarryOS |

The fastest local row used this explicit feature set:

```text
FEATURES=plat-dyn,cntv-timer,smp,ax-feat/display,ax-feat/rtc,ax-driver/virtio-blk,ax-driver/virtio-net,ax-driver/virtio-gpu,ax-driver/virtio-input,ax-driver/virtio-socket,starry-kernel/input,starry-kernel/vsock
```

Use explicit end-to-end ratios when reporting the full guest cargo build. The
checked-in numbers support these ratios:

```text
951s / 331s = 2.87x   slow guest baseline to tuned local best
642s / 331s = 1.94x   tmp source/target baseline to tuned local best
422s / 331s = 1.28x   tuned JOBS=1 to tuned JOBS=8
```

## Interpretation

The self-build already passes in an 8-vCPU StarryOS guest. The remaining gap to
the host reference is not just "more CPUs"; it is the sum of filesystem
writeback, process/wait/pipe overhead, SMP scheduling, lock contention, and
serial Cargo critical-path work.
