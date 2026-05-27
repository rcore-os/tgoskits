# llama.cpp Alpine/musl Compatibility Report v0.3

## 1. Goal

Run llama.cpp on StarryOS with Alpine/musl userspace using
CPU-only, single-thread, tiny-model, `--no-mmap` inference.

## 2. Environment

### Common

- tgoskits commit: `c6cfdc03d` (branch `feat/starry-llama-alpine-compat`)
- llama.cpp version: b5092 (`d3bd7193ba66c15963fd1c59448f22019a8caf6e`)
- build flags: `-DLLAMA_CURL=OFF -DCMAKE_BUILD_TYPE=Release -DGGML_NATIVE=OFF -DGGML_OPENMP=OFF` (static, CPU-only)
- model: SmolLM2-135M Q4_0 (87.5MB)
- QEMU memory: 512MB (framework constraint)

### aarch64

- Alpine rootfs: `rootfs-aarch64-alpine.img` (1GB sparse)
- build toolchain: aarch64-linux-musl-gcc 11.2.1
- llama-cli binary: 15.6MB static PIE
- QEMU CPU: cortex-a53

### x86_64

- Alpine rootfs: `rootfs-x86_64-alpine.img` (1GB sparse)
- build toolchain: x86_64-linux-musl-gcc 11.2.1
- llama-cli binary: 16MB static PIE
- QEMU CPU: max (required for SSE4.2 support)
- build dir: `build-x86_64` (independent from aarch64 `build/`)

### riscv64

- Alpine rootfs: `rootfs-riscv64-alpine.img` (1GB sparse)
- build toolchain: riscv64-linux-musl-gcc 11.2.1
- llama-cli binary: 3.4MB static (non-PIE, `-no-pie -fno-pie -static`)
- QEMU CPU: rv64
- build dir: `build-riscv64-static` (non-PIE, GGML_RVV=OFF)
- **Note**: static-PIE causes segfault on StarryOS riscv64; non-PIE static required

### loongarch64

- Alpine rootfs: `rootfs-loongarch64-alpine.img` (1GB sparse, available)
- **Status**: BLOCKED — musl.cc has no loongarch64 toolchain (HTTP 404)

## 3. Test Results

### aarch64 (Full Verification Run)

| Case | Level | Command | Result | QEMU Time | Notes |
|------|-------|---------|--------|-----------|-------|
| llama-cpp-help | L0 | `llama-cli --help` | PASS | 10.21s | RC=0, help output normal |
| llama-cpp-init | L1 | `llama-cli -m /nonexistent.gguf -p "hi" -n 1 -t 1` | PASS | 9.98s | RC=1, graceful error (negative test) |
| llama-cpp-load | L2/L3 | `llama-cli -m model --no-mmap -n 1 -t 1 -c 256` | PASS | 22.68s | RC=0, model loaded via fread |
| llama-cpp-infer | L4 | `llama-cli -m model --no-mmap -n 8 -t 1 -c 512` | PASS | 27.65s | RC=0, 8 tokens generated |

#### aarch64 L4 Performance

```
load time       = 10903 ms
prompt eval     =    0 ms /   1 tokens (inf tok/s)
eval time       = 5758 ms /   8 runs  (719 ms/tok, 1.39 tok/s)
total time      = 5982 ms /   9 tokens
```

### x86_64 (Full Verification Run)

| Case | Level | Command | Result | QEMU Time | Notes |
|------|-------|---------|--------|-----------|-------|
| llama-cpp-help | L0 | `llama-cli --help` | PASS | 9.40s | RC=0, help output normal |
| llama-cpp-init | L1 | `llama-cli -m /nonexistent.gguf -p "hi" -n 1 -t 1` | PASS | 9.70s | RC=1, graceful error (negative test) |
| llama-cpp-load | L2/L3 | `llama-cli -m model --no-mmap -n 1 -t 1 -c 256` | PASS | 29.67s | RC=0, model loaded via fread |
| llama-cpp-infer | L4 | `llama-cli -m model --no-mmap -n 8 -t 1 -c 512` | PASS | 43.61s | RC=0, 8 tokens generated |

#### x86_64 L4 Performance

```
load time       = 10286 ms
prompt eval     =    0 ms /   1 tokens (inf tok/s)
eval time       = 13399 ms /   8 runs  (1675 ms/tok, 0.60 tok/s)
total time      = 13566 ms /   9 tokens
```

### riscv64 (Full Verification Run)

| Case | Level | Command | Result | QEMU Time | Notes |
|------|-------|---------|--------|-----------|-------|
| llama-cpp-help | L0 | `llama-cli --help` | PASS | 2.87s | RC=0, help output normal |
| llama-cpp-init | L1 | `llama-cli -m /nonexistent.gguf -p "hi" -n 1 -t 1` | PASS | 3.18s | RC=1, graceful error (negative test) |
| llama-cpp-load | L2/L3 | `llama-cli -m model --no-mmap -n 1 -t 1 -c 256` | PASS | 20.21s | RC=0, model loaded via fread |
| llama-cpp-infer | L4 | `llama-cli -m model --no-mmap -n 8 -t 1 -c 512` | PASS | 30.94s | RC=0, 8 tokens generated |

#### riscv64 L4 Performance

```
load time       = 14894 ms
prompt eval     =    0 ms /   1 tokens (inf tok/s)
eval time       = 11182 ms /   8 runs  (1398 ms/tok, 0.72 tok/s)
total time      = 11458 ms /   9 tokens
```

### Cross-Architecture Performance Comparison

| Metric | aarch64 (cortex-a53) | x86_64 (max) | riscv64 (rv64) |
|--------|---------------------|--------------|----------------|
| Binary size | 15.6MB | 16MB | 3.4MB |
| Load time | 10.9s | 10.3s | 14.9s |
| Eval speed | 1.39 tok/s | 0.60 tok/s | 0.72 tok/s |
| Total time | 6.0s / 9 tok | 13.6s / 9 tok | 11.5s / 9 tok |

- x86_64 is slower due to SSE/AVX instruction emulation overhead in QEMU TCG
- riscv64 binary is much smaller (3.4MB vs 15-16MB) due to GGML_RVV=OFF removing SIMD code paths
- riscv64 load time is longer but eval speed is comparable to x86_64

## 4. Issues Found

| ID | Level | Arch | Symptom | Root Cause | Fix | Status |
|----|-------|------|---------|------------|-----|--------|
| 1 | L0+ | both | stress tests panic: root device not found | stress build config missing `ax-driver/virtio-blk` | Added 7 ax-driver features to build-*.toml | Fixed |
| 2 | L2 | x86_64 | IllegalInstruction at ip=0x212ca3 | cmake auto-detected SSE4.2 on host; QEMU default CPU (qemu64) only supports SSE2 | Added `-cpu max` to QEMU args | Fixed |
| 3 | L0 | x86_64 | shell_prefix timeout | Used `starry:~#` from stress-ng-0 config; actual prompt is `root@starry:/root #` | Changed to `root@starry:` | Fixed |
| 4 | build | riscv64 | cmake build error: `cannot find default versions of the ISA extension 'v'` | GGML_RVV auto-detection enabled RVV but musl toolchain lacks V extension support | Added `-DGGML_RVV=OFF` | Fixed |
| 5 | L0 | riscv64 | segfault (RC=139) on static-PIE binary | StarryOS riscv64 kernel does not support static-PIE (Type=DYN) ELF execution | Rebuilt with `-no-pie -fno-pie -static` to produce Type=EXEC binary | Fixed |

## 5. Syscall Notes

llama-cli startup, model load, and inference exercised the following
syscall categories (observed via strace on Linux reference):
- file I/O: `openat`, `read`, `write`, `close`, `fstat`, `lseek`
- memory: `mmap` (anonymous), `mprotect`, `brk` (heap growth via musl)
- process: `exit_group`, `getpid`, `sched_yield`
- threading (even `-t 1`): `clone`, `futex`, `mmap` for thread stacks
- `--no-mmap` avoids file-backed mmap, uses fread instead

Full syscall trace capture requires enabling StarryOS syscall logging,
not performed in v0.3.

## 6. Configuration Files

### aarch64

- `build-aarch64-unknown-none-softfloat.toml` (ax-hal/aarch64-qemu-virt)
- `llama-cpp-help/qemu-aarch64.toml` (L0, 60s, `-cpu cortex-a53`)
- `llama-cpp-init/qemu-aarch64.toml` (L1, 120s)
- `llama-cpp-load/qemu-aarch64.toml` (L2/L3, 300s)
- `llama-cpp-infer/qemu-aarch64.toml` (L4, 600s)

### x86_64

- `build-x86_64-unknown-none.toml` (ax-hal/x86-pc)
- `llama-cpp-help/qemu-x86_64.toml` (L0, 60s, `-cpu max`)
- `llama-cpp-init/qemu-x86_64.toml` (L1, 120s)
- `llama-cpp-load/qemu-x86_64.toml` (L2/L3, 300s)
- `llama-cpp-infer/qemu-x86_64.toml` (L4, 600s)

### x86_64-specific configuration notes

- `to_bin = false` (x86_64 QEMU boots ELF directly, no bin conversion)
- `shell_prefix = "root@starry:"` (same as aarch64, different from stress-ng-0's `starry:~#`)
- `-cpu max` required for SSE4.2 instruction support

### riscv64

- `build-riscv64gc-unknown-none-elf.toml` (ax-hal/riscv64-qemu-virt)
- `llama-cpp-help/qemu-riscv64.toml` (L0, 60s, `-cpu rv64`)
- `llama-cpp-init/qemu-riscv64.toml` (L1, 120s)
- `llama-cpp-load/qemu-riscv64.toml` (L2/L3, 300s)
- `llama-cpp-infer/qemu-riscv64.toml` (L4, 600s)

### riscv64-specific configuration notes

- `to_bin = true` (riscv64 QEMU requires binary conversion)
- `shell_prefix = "root@starry:"` (same as aarch64)
- `-cpu rv64` (standard RISC-V 64-bit, no V extension)
- Binary must be non-PIE static (Type=EXEC), not static-PIE (Type=DYN)
- GGML_RVV=OFF required (musl toolchain lacks V extension intrinsics)

## 7. Resource Observation under 512MB QEMU

- Rootfs size: 1GB (sparse) per architecture
- llama-cli: 15.6MB (aarch64), 16MB (x86_64), 3.4MB (riscv64, non-PIE, no SIMD)
- Model file: 87.5MB (shared across architectures)
- Model load (fread, --no-mmap): ~88MB heap allocation
- Context: 512 tokens (-c 512)
- Token generation: 8 tokens (-n 8)
- OOM: not observed on any architecture
- Conclusion: SmolLM2-135M Q4_0 fits comfortably in 512MB guest with
  Alpine rootfs on all three tested architectures.

## 8. Current Limitations

- **Debian/glibc**: not covered (Alpine/musl only)
- **mmap file mapping**: not tested (`--no-mmap` used throughout)
- **dynamic linking musl**: not tested (static binary used)
- **multi-thread**: not tested (`-t 1` only)
- **riscv64 SIMD (RVV)**: disabled (GGML_RVV=OFF), scalar-only path
- **loongarch64**: blocked — musl.cc has no loongarch64 toolchain
- **large model**: not tested (only 135M Q4_0)
- **QEMU memory**: fixed at 512MB; larger-memory behavior not evaluated
- **riscv64 static-PIE**: StarryOS riscv64 kernel does not support static-PIE (Type=DYN) ELF

## 9. Next Steps

- Test loongarch64 architecture (pending toolchain availability)
- Test mmap file mapping path (remove `--no-mmap`)
- Test larger model (e.g., SmolLM2-360M or 1B)
- Test multi-thread (`-t 2`, `-t 4`)
- Test dynamic musl linking
- Investigate riscv64 static-PIE support (kernel ELF loader)
- Enable RVV for riscv64 if QEMU V extension emulation is supported
- Debian/glibc compatibility
- Integrate into CI pipeline
