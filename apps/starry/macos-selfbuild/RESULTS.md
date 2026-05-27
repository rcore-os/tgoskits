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

## Build Command Shape

Inside the StarryOS guest, the app runs:

```bash
cargo build \
  -p starryos \
  --bin starryos \
  --target aarch64-unknown-none-softfloat \
  -Z build-std=core,alloc,compiler_builtins \
  --target-dir /tmp/starryos-selfbuild-target \
  --features qemu,gic-v3,cntv-timer,smp \
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
FEATURES=ax-feat/defplat,ax-feat/irq,ax-feat/ipi,ax-feat/rtc,ax-feat/bus-pci,gic-v3,cntv-timer,smp
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
