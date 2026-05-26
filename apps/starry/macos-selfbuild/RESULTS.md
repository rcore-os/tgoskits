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

## Reference Numbers

| Case | Time | Notes |
| --- | --- | --- |
| `SMP=8`, `JOBS=1`, ext4 source/target | `951s` | slow guest baseline |
| `SMP=8`, `JOBS=8`, ext4 source/target | `917s` | first complete SMP self-build |
| `SMP=8`, `JOBS=8`, tmpfs source/target | `660s` | removes heavy ext4 output pressure |
| `SMP=8`, `JOBS=8`, tmpfs, `-Z threads=2` | `331s` | fastest local full self-build |
| host macOS oracle | `134s` | same target on host, not inside StarryOS |

## Interpretation

The self-build already passes in an 8-vCPU StarryOS guest. The remaining gap to
the host oracle is not just "more CPUs"; it is the sum of filesystem writeback,
process/wait/pipe overhead, SMP scheduling, lock contention, and serial Cargo
critical-path work.
