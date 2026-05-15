---
doc_type: TESTING
scope: Testing infrastructure and practices
---
# Testing Patterns

**Analysis Date:** 2026-05-13

## Test Infrastructure

### Build System Entry Point

All testing goes through `cargo xtask`. The root xtask (`tg-xtask` at `xtask/`) delegates to the `axbuild` crate at `scripts/axbuild/`. Never use raw `cargo test`.

```bash
# Full regression suite
cargo xtask test

# System-specific QEMU tests
cargo xtask arceos test qemu --arch aarch64
cargo xtask starry test qemu --arch riscv64
cargo xtask axvisor test qemu --arch x86_64

# Board (physical hardware) tests
cargo xtask starry test board --board orangepi-5-plus
cargo xtask axvisor test board --board orangepi-5-plus-linux

# Run a specific test case
cargo xtask starry test qemu --arch riscv64 -g normal -c smoke
cargo xtask starry test qemu --arch x86_64 -c affinity

# List discovered test cases
cargo xtask starry test qemu -l
cargo xtask starry test board -l
```

### CI Environment

Tests run in Docker containers defined at `container/`:
- `container/Dockerfile` — base CI image with QEMU 10.2.1, cross-compilation toolchains, and QEMU user-mode for rootfs staging
- `container/Dockerfile.axvisor-lvz` — extended image for LoongArch virtualization

The base image includes cross-compilers for all four target architectures (`x86_64-linux-musl-cross`, `aarch64-linux-musl-cross`, `riscv64-linux-musl-cross`, `loongarch64-linux-musl-cross`).

Container images are published to `ghcr.io/rcore-os/tgoskits-container` and are automatically rebuilt when `container/Dockerfile` or `rust-toolchain.toml` changes on `main` or `dev`.

## Test Suites

### ArceOS Tests (`test-suit/arceos/`)

Two categories:
- `test-suit/arceos/c/` — C language tests
- `test-suit/arceos/rust/` — Rust integration tests (organized by subsystem: `display`, `exception`, `fs`, `memtest`, `net`, `task`)

#### C Test Structure

Each C test is a directory under `test-suit/arceos/c/`:
```
test-suit/arceos/c/
  helloworld/
    main.c                      # Test source
    test_cmd                    # Shell commands to run
    expect_info.out             # Expected output (golden file)
    expect_info_smp4.out        # Expected output for SMP=4 variant
    helloworld_*.bin            # Prebuilt binary
    helloworld_*.elf            # Prebuilt ELF
    build_riscv64/              # Per-arch build artifacts
    build_x86_64/
  httpclient/
    main.c
    test_cmd
    expect_info.out
    features.txt                # Required feature flags
    axbuild.mk                  # Build configuration
    ...
  memtest/
    ...
  pthread/
    basic/
    parallel/
    pipe/
    sleep/
```

The `test_cmd` file defines invocation lines using `test_one`:
```sh
test_one "LOG=info" "expect_info.out"
test_one "SMP=4 LOG=info" "expect_info_smp4.out"
```

The `expect_info.out` golden file contains the expected stdout output, including the `Shutting down...` termination message.

### StarryOS Tests (`test-suit/starryos/`)

#### Directory Structure

```
test-suit/starryos/
  normal/                          # CI-passing tests
    qemu-smp1/                     # Single-core QEMU tests
      build-<target>.toml          # Build config per target
      smoke/                       # Smoke test case
        qemu-<arch>.toml           # Runtime config
      busybox/                     # Busybox applet test
        qemu-<arch>.toml
        sh/busybox-tests.sh
      syscall/                     # Syscall tests (grouped C subcases)
        qemu-<arch>.toml
        <subcase>/c/CMakeLists.txt
      apk-curl/
      apt/
      bugfix/
      python-hello/
      ...
    qemu-smp4/                     # Multi-core QEMU tests
      build-<target>.toml
      affinity/
        qemu-<arch>.toml
      test-shm-deadlock/
    qemu-aarch64-plat-dyn/         # aarch64 dynamic platform tests
    qemu-dhcp/                     # DHCP network tests
    board-orangepi-5-plus/         # Physical board tests
      build-aarch64-unknown-none-softfloat.toml
      boot/
      lsusb/
      net-smoke/
      npu-yolov8/
      pcie-enumerate/
  stress/                          # Heavy/long-running tests
    postgresql/
    stress-ng-0/
```

#### Test Pipelines

The test runner selects a pipeline based on case directory contents:

| Pipeline | Trigger Condition | Behavior |
|----------|------------------|----------|
| `plain` | No `test_commands`, no `c/`, `sh/`, or `python/` | Boot shared rootfs with QEMU `-snapshot` |
| `c` | Case contains `c/` directory | CMake cross-compile, install to rootfs overlay |
| `sh` | Case contains `sh/` directory | Inject shell script into `/usr/bin/` |
| `python` | Case contains `python/` directory | Install `python3` in staging rootfs, inject `.py` files |
| `grouped` | `qemu-<arch>.toml` has `test_commands` | Build all C subcases, generate sequential test runner |

#### QEMU TOML Configuration

Each `qemu-<arch>.toml` file defines runtime configuration:

| Field | Description |
|-------|-------------|
| `args` | QEMU arguments (`${workspace}` resolves to repo root) |
| `uefi` | Whether to use UEFI boot |
| `to_bin` | Convert ELF to bare binary |
| `shell_prefix` | Guest shell prompt string (e.g., `"root@starry:"`) |
| `shell_init_cmd` | Guest command for plain/C/sh/python cases |
| `test_commands` | Guest command list for grouped cases (cannot combine with `shell_init_cmd`) |
| `success_regex` | All patterns must match for PASS |
| `fail_regex` | Any match causes FAIL |
| `timeout` | Timeout in seconds |

Example (`smoke/qemu-riscv64.toml`):
```toml
args = [
    "-nographic", "-m", "512M", "-cpu", "rv64",
    "-device", "virtio-blk-pci,drive=disk0",
    "-drive", "id=disk0,if=none,format=raw,file=${workspace}/target/rootfs/rootfs-riscv64-alpine.img",
    "-device", "virtio-net-pci,netdev=net0",
    "-netdev", "user,id=net0",
]
uefi = false
to_bin = true
shell_prefix = "root@starry:"
shell_init_cmd = "pwd && echo 'All tests passed!'"
success_regex = ["(?m)^All tests passed!\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b']
timeout = 5
```

#### C Test Case Structure

```
<case>/
  qemu-<arch>.toml
  c/
    CMakeLists.txt
    prebuild.sh        # Optional: install extra packages in staging rootfs
    src/
      main.c
```

`CMakeLists.txt` uses CMake with `C_STANDARD 11`, `-Wall -Wextra -Werror`, and installs to `usr/bin`.

#### Shell and Python Cases

Shell case (`sh/` directory):
```
<case>/
  qemu-<arch>.toml
  sh/
    my-test.sh
```

Python case (`python/` directory):
```
<case>/
  qemu-<arch>.toml
  python/
    test_hello.py
```

#### Grouped Cases

When multiple guest programs share one StarryOS boot:
```
<case>/
  qemu-<arch>.toml
  <subcase-a>/c/CMakeLists.txt
  <subcase-b>/c/CMakeLists.txt
```

Uses `test_commands` in the TOML instead of `shell_init_cmd`:
```toml
test_commands = ["/usr/bin/test-a", "/usr/bin/test-b"]
success_regex = ["(?m)^STARRY_GROUPED_TESTS_PASSED\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b', '(?m)^STARRY_GROUPED_TEST_FAILED:']
```

#### Build Config TOML

`build-<target>.toml` or `build-<arch>.toml` defines build parameters:
```toml
target = "aarch64-unknown-none-softfloat"
env = {}
features = ["ax-feat/bus-mmio", "ax-feat/bus-pci", "starry-kernel/plat-dyn", ...]
log = "Info"
max_cpu_num = 8
plat_dyn = true
```

### Axvisor Tests (`test-suit/axvisor/`)

Two groups:
- `test-suit/axvisor/normal/` — standard tests with board and QEMU variants
- `test-suit/axvisor/svm/` — SVM (Secure Virtual Machine) tests

Board targets include:
- `board-orangepi-5-plus` — Orange Pi 5 Plus
- `board-phytiumpi` — Phytium Pi
- `board-rdk-s100` — RDK S100
- `board-roc-rk3568-pc` — ROC-RK3568-PC

## Quality Gates

### CI Checks (in order)

1. **Format check** (`cargo fmt --all -- --check`) — runs first, blocks all downstream checks
2. **Clippy** (`cargo xtask clippy`) — all crates listed in `scripts/test/clippy_crates.csv`
3. **Sync-lint** (`cargo xtask sync-lint`) — verifies `clippy_crates.csv` and `std_crates.csv` match the actual crate graph
4. **Test with std** (`cargo xtask test`) — runs std-mode tests for crates in `scripts/test/std_crates.csv`
5. **System QEMU tests** — per-architecture for arceos, starry, axvisor
6. **Stress tests** (main PRs only) — `echo "TODO!"` (not yet implemented)

### Tracking Files

**`scripts/test/clippy_crates.csv`** — lists every crate that must pass clippy. Contains ~103 crate names. When adding a new crate, run clippy on it and add it to this file.

**`scripts/test/std_crates.csv`** — lists ~53 crates eligible for std-mode host testing. These are infrastructure crates (no bare-metal dependencies) like `ax-errno`, `ax-allocator`, `ax-sync`, `starry-process`, etc.

### Running Quality Checks Locally

```bash
cargo fmt --all -- --check
cargo xtask clippy
cargo xtask sync-lint
cargo xtask test
cargo xtask arceos test qemu --arch aarch64
cargo xtask starry test qemu --arch riscv64
cargo xtask axvisor test qemu --arch x86_64
```

## CI Pipeline

### GitHub Actions Workflow

`.github/workflows/ci.yml` is the main CI workflow (triggered on push/PR to paths excluding docs).

**Path-based change detection:** Uses `dorny/paths-filter@v4` to skip CI when only documentation or non-code paths change. On push to `main` or `dev`, always runs full CI.

**Concurrency group:** `ci-${{ github.workflow }}-${{ github.ref }}`, cancel-in-progress for push events.

### Job Matrix

| Job | Command | Runner | Container |
|-----|---------|--------|-----------|
| Check formatting | `cargo fmt --all -- --check` | ubuntu-latest | base |
| Run clippy | `cargo xtask clippy` | ubuntu-latest | base |
| Run sync-lint | `cargo xtask sync-lint` | ubuntu-latest | base |
| Test with std | `cargo xtask test` | ubuntu-latest | base |
| Test axvisor aarch64 | `cargo xtask axvisor test qemu --arch aarch64` | ubuntu-latest | base |
| Test axvisor riscv64 | `cargo xtask axvisor test qemu --arch riscv64` | ubuntu-latest | base |
| Test axvisor loongarch64 | `cargo xtask axvisor test qemu --arch loongarch64` | ubuntu-latest | axvisor-lvz |
| Test starry riscv64 | `cargo xtask starry test qemu --arch riscv64` | ubuntu-latest | base |
| Test starry aarch64 | `cargo xtask starry test qemu --arch aarch64` | ubuntu-latest | base |
| Test starry loongarch64 | `cargo xtask starry test qemu --arch loongarch64` | ubuntu-latest | base |
| Test starry x86_64 | `cargo xtask starry test qemu --arch x86_64` | ubuntu-latest | base |
| Test arceos x86_64 | `cargo xtask arceos test qemu --arch x86_64` | ubuntu-latest | base |
| Test arceos riscv64 | `cargo xtask arceos test qemu --arch riscv64` | ubuntu-latest | base |
| Test arceos aarch64 | `cargo xtask arceos test qemu --arch aarch64` | ubuntu-latest | base |
| Test arceos loongarch64 | `cargo xtask arceos test qemu --arch loongarch64` | ubuntu-latest | base |
| Stress starry aarch64/x86_64 | `echo "TODO!"` | ubuntu-latest | base |
| Test axvisor self-hosted x86_64 | `cargo xtask axvisor test qemu --arch x86_64` | self-hosted (Intel) | none |
| Test axvisor x86_64 svm | `cargo xtask axvisor test qemu --arch x86_64 --test-group svm --test-case smoke` | ubuntu-latest | none |
| Test axvisor board orangepi-5-plus | `cargo xtask axvisor test board --board orangepi-5-plus-linux` | self-hosted (board) | none |
| Test starry board orangepi-5-plus | `cargo xtask starry test board --board orangepi-5-plus` | self-hosted (board) | none |

### Architecture Coverage

All four target architectures are tested:
- **x86_64** (`x86_64-unknown-none`)
- **aarch64** (`aarch64-unknown-none-softfloat`)
- **riscv64** (`riscv64gc-unknown-none-elf`)
- **loongarch64** (`loongarch64-unknown-none-softfloat`)

### Board Testing

Self-hosted runners with physical hardware:
- **Orange Pi 5 Plus** — tests both StarryOS and Axvisor
- **Intel host** — x86_64 SVM testing (AXsvisor nested virtualization x86)
- Requires `github.repository_owner == 'rcore-os'` for self-hosted jobs

### Release Publishing

Releases publish automatically via `release-plz` on push to `dev` when all CI checks pass. Configuration at `release-plz.toml`:
```toml
[workspace]
publish_no_verify = true
```

## Test Patterns

### AAA Pattern

For unit tests (std-mode tests for infrastructure crates), use the Arrange-Act-Assert pattern. Tests are written as `#[test]` functions in `#[cfg(test)]` modules within each crate.

### Success/Fail Regex in TOML

StarryOS tests use regex-based pass/fail detection:
- `success_regex` — ALL patterns must match in guest output for PASS
- `fail_regex` — ANY match causes immediate FAIL
- Prefer stable, unique success lines (e.g., `All tests passed!`)
- Keep `fail_regex` precise to avoid false positives (avoid matching normal output like `failed: 0`)

### Timeout-Based Stability

All StarryOS QEMU tests use a `timeout` field (seconds). Set a reasonable timeout for each case — too short causes flaky failures, too long wastes CI time. Typical values: 5-15 seconds for simple cases, 60+ for heavy tests.

### Test Discovery

Tests are discovered by the axbuild test runner scanning the `test-suit/` directory:
- QEMU cases: `<case>/qemu-<arch>.toml`
- Board cases: `<case>/board-<board>.toml`
- Build configs: `<case>/build-<target>.toml` or found from parent build wrapper directory
- Cases without matching runtime config for the target architecture are skipped

### Test Maintenance Rules

- Only add `qemu-<arch>.toml` for architectures that have been verified to pass.
- `qemu-smp1` / `qemu-smp4` concurrency is set by build config — update both QEMU `-smp` and build config together.
- A case can only use one pipeline (don't mix `c/`, `sh/`, `python/`, `test_commands`).
- Don't run multiple `cargo xtask starry test qemu` instances in parallel in the same workspace — rootfs and generated configs may conflict.
- `normal/` group should stay CI-stable. Add heavy/slow tests to `stress/` or new custom groups.

---

*Testing analysis: 2026-05-13*
