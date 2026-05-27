# llama.cpp Alpine/musl Compatibility

Tests llama.cpp (b5092) Alpine/musl static binary compatibility on StarryOS.
Covers binary execution, error handling, model loading, and token generation.

## Prerequisites

- llama-cli binary and SmolLM2-135M Q4_0 model pre-injected into the Alpine rootfs
- Rootfs images at `tmp/axbuild/rootfs/rootfs-{arch}-alpine.img`

## Running

```bash
cargo xtask starry app run -t llama-cpp --arch aarch64
cargo xtask starry app run -t llama-cpp --arch x86_64
cargo xtask starry app run -t llama-cpp --arch riscv64
```

## Test Levels

The unified test script runs L0-L4 sequentially:

| Level | Test | What it verifies |
|-------|------|-----------------|
| L0 | `--help` | Binary executes, exits 0 |
| L1 | missing model | Graceful error (RC!=0, output contains error keywords) |
| L2/L3 | model load | `--no-mmap` fread loads Q4_0 model, generates 1 token |
| L4 | inference | Full pipeline: load + generate 8 tokens |

## Stability Check (L5)

Each architecture: init x3 + infer x3 = 6 runs.

| Arch | init x3 | infer x3 | Notes |
|------|---------|----------|-------|
| aarch64 | 2/3 PASS | 3/3 PASS | 1 init FAIL: kernel kretprobe selftest panic (unrelated) |
| x86_64 | 3/3 PASS | 3/3 PASS | |
| riscv64 | 3/3 PASS | 3/3 PASS | |

Application-level stability: 100% (17/18 PASS, 1 kernel issue).

## Architecture Notes

| Arch | CPU flag | to_bin | Binary | Notes |
|------|----------|--------|--------|-------|
| aarch64 | cortex-a53 | true | 15.6MB static PIE | baseline |
| x86_64 | max | false | 16MB static PIE | cmake auto-enables SSE4.2 |
| riscv64 | rv64 | true | 3.4MB non-PIE static | GGML_RVV=OFF, static-pie segfaults |

## L4 Performance (SmolLM2-135M Q4_0, --no-mmap -t 1)

| Arch | Load | Eval | tok/s |
|------|------|------|-------|
| aarch64 | 10.9s | 5.76s/8tok | 1.39 |
| x86_64 | 10.3s | 13.4s/8tok | 0.60 |
| riscv64 | 14.9s | 11.2s/8tok | 0.72 |

## Build Config

Features: `ax-hal/<arch>`, `qemu`, `ax-driver/pci`, `ax-driver/virtio-blk`, `ax-driver/virtio-net`, `ax-driver/virtio-gpu`, `ax-driver/virtio-input`, `ax-driver/virtio-socket`.

x86_64 QEMU requires `-cpu max` for SSE4.2 support.
