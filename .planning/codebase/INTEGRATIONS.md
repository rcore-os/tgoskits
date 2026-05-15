---
doc_type: INTEGRATIONS
scope: External integrations and CI/CD pipeline
analysis_date: 2026-05-13
---
# Integrations

## CI/CD Pipeline

**Platform:** GitHub Actions

**Workflow files (in `.github/workflows/`):**

| File | Purpose | Triggers |
|------|---------|----------|
| `ci.yml` (511 lines) | Main CI pipeline: fmt, clippy, sync-lint, multi-arch tests, container publish, release | Push (selective paths), PR, `workflow_dispatch` |
| `container-publish.yml` (74 lines) | Reusable workflow: build and push Docker images to ghcr.io | Called by `ci.yml` |
| `reusable-command.yml` (158 lines) | Reusable workflow: run a command on host or inside container with caching | Called by `ci.yml` |
| `release-plz.yml` (47 lines) | Automated release PR creation on `main` | Push to `main` |
| `docs.yml` (66 lines) | Docusaurus site build and deploy to GitHub Pages | Push to `main` (only `docs/**` changes) |

**CI Pipeline Steps (on push/PR to dev/main):**

1. **detect_changes** -- Path filter to determine which jobs to run
2. **fmt** -- `cargo fmt --all -- --check`
3. **post_fmt_checks** (27-job matrix) -- Run in parallel:
   - Core quality: clippy, sync-lint, test-std
   - ArceOS tests: x86_64, riscv64, aarch64, loongarch64 (QEMU)
   - Axvisor tests: aarch64, riscv64 (QEMU), loongarch64 (QEMU-LVZ), x86_64 (self-hosted Intel), x86_64 SVM (hosted), Orange Pi 5+ (self-hosted board)
   - StarryOS tests: x86_64, riscv64, aarch64, loongarch64 (QEMU), Orange Pi 5+ (self-hosted board)
   - Stress tests: StarryOS across all 4 architectures (marked TODO)
4. **Container publish** (on push to main/dev, when Dockerfile changes)
5. **Release publish** (on push to dev in `rcore-os/tgoskits`, when all checks pass)

**Runner Types:**
- `ubuntu-latest` -- standard GitHub-hosted (most jobs)
- `self-hosted` with label `linux, intel` -- Axvisor x86_64 tests requiring KVM (restricted to `rcore-os` owner)
- `self-hosted` with label `linux, board` -- Orange Pi 5 Plus physical board tests (restricted to `rcore-os` owner)

**Caching:**
- Rust caches via `Swatinem/rust-cache@v2` with per-job `shared-key` (e.g., `clippy`, `test-arceos-aarch64`)
- Docker build cache via GitHub Actions cache (`type=gha`)

**Concurrency:**
- CI workflow: `cancel-in-progress: true` (per branch)
- Release workflow: `cancel-in-progress: false` (per branch)

**Permissions:**
- CI jobs: `contents: read`, `packages: read`
- Container publish: `contents: read`, `packages: write`
- Release: `contents: write`, `pull-requests: read`
- Docs: `contents: read`, `pages: write`, `id-token: write`

## External Services

**GitHub Container Registry (ghcr.io):**
- Docker images published to `ghcr.io/rcore-os/tgoskits-container` and `ghcr.io/rcore-os/tgoskits-container-axvisor-lvz`
- Authentication via `GITHUB_TOKEN` (automatic)
- Tags: `latest` (raw tag), ref tags on tag events

**GitHub Pages:**
- Docusaurus documentation site at `https://rcore-os.github.io/tgoskits/`
- Built from `docs/` directory using Node.js 20 + Yarn
- Deployment branch: `gh-pages`

**crates.io (via release-plz):**
- Automated crate publishing via `release-plz/action@v0.5`
- Config: `release-plz.toml` (`publish_no_verify = true`)
- Secrets required: `CARGO_REGISTRY_TOKEN`
- Only runs on `rcore-os/tgoskits`

**Upstream Repository Tracking:**
- 60+ component repositories tracked via git-subtree in `components/`
- Managed by `scripts/repo/repo.py` using `scripts/repo/repos.csv`
- Upstream organizations: `arceos-org`, `arceos-hypervisor`, `Starry-OS`, `rcore-os`, `drivercraft`, `DeathWish5`

**Prebuilt Toolchain Downloads:**
- musl cross-compilers for aarch64, riscv64, x86_64: from `arceos-org/setup-musl` (GitHub Releases) or `musl.cc`
- musl cross-compiler for loongarch64: from both `arceos-org/setup-musl` and `LoongsonLab/oscomp-toolchains-for-oskernel` (fallback)
- QEMU source: `https://download.qemu.org/qemu-${QEMU_VERSION}.tar.xz`
- QEMU-LVZ source: `numpy1314/QEMU-LVZ` (GitHub, pinned to commit `f82f6aee`)

## Build Artifacts

**Docker Images (built from `container/`):**

| Image | Dockerfile | Contents |
|-------|-----------|----------|
| Base (`ghcr.io/rcore-os/tgoskits-container`) | `container/Dockerfile` | Ubuntu 24.04 + Rust nightly toolchain + QEMU 10.2.1 (system + user emulators) + 4 musl cross-compilers + cargo-binutils, axconfig-gen, cargo-axplat, strace |
| Axvisor-LVZ (`ghcr.io/rcore-os/tgoskits-container-axvisor-lvz`) | `container/Dockerfile.axvisor-lvz` | Base image + custom QEMU-LVZ build (loongarch64-softmmu only) |

**Docker Image Build Process (`container/Dockerfile`):**
1. Base: `ubuntu:24.04`
2. Install build tools: `clang`, `cmake`, `meson`, `ninja-build`, `python3-venv`, `qemu-user-static`, rootfs tools (`dosfstools`, `e2fsprogs`)
3. Build QEMU 10.2.1 from source (all 4 architectures: system + user emulators)
4. Install 4 musl cross-compilers from prebuilt tarballs
5. Install Rust nightly-2026-04-27 via rustup, then `cargo install cargo-binutils axconfig-gen cargo-axplat`
6. Install strace for debugging

**RootFS Images:**
- StarryOS: `.ext4` root filesystem images built by `cargo xtask starry rootfs --arch <arch>`
- Managed by `scripts/axbuild/src/rootfs/` and `scripts/axbuild/src/starry/rootfs.rs`
- Alpine Linux-based, APK packages fetched and injected into the image
- Configurable APK region via `STARRY_APK_REGION` env var (default: `china`, CI uses `us` for QEMU test jobs)

**Per-Architecture Build Output:**
- `target/<target-triple>/debug/` or `target/<target-triple>/release/`
- Debug symbols used by CodeLLDB via GDB remote protocol to QEMU

## Test Infrastructure

**Test Commands (via `cargo xtask`):**

```bash
cargo xtask test                          # std tests (53 crates whitelist)
cargo xtask arceos test qemu --arch <arch>  # ArceOS per-arch QEMU tests
cargo xtask starry test qemu --arch <arch>  # StarryOS per-arch QEMU tests
cargo xtask axvisor test qemu --arch <arch> # Axvisor per-arch QEMU tests
cargo xtask axvisor test board --board orangepi-5-plus-linux  # Physical board test
cargo xtask starry test board --board orangepi-5-plus         # Physical board test
```

**QEMU-based Testing:**
- Each architecture tested with matching QEMU system emulator
- QEMU user emulators (`qemu-aarch64-static`, etc.) used for StarryOS rootfs preparation inside containers
- LoongArch hypervisor tests use custom QEMU-LVZ build (`AXBUILD_QEMU_SYSTEM_LOONGARCH64` env var)
- x86_64 SVM tests run on GitHub-hosted runners with `kvm` group access

**Physical Board Testing (Orange Pi 5 Plus):**
- Self-hosted runner with `linux, board` labels
- Restricted to `rcore-os` organization (via `required_repository_owner` check)
- Orchestrated via `cargo xtask axvisor test board --board orangepi-5-plus-linux` and `cargo xtask starry test board --board orangepi-5-plus`
- Uses `ostool` for remote board management

**Test Suite Structure (`test-suit/`):**
- `test-suit/arceos/` -- ArceOS Rust tests (display, exception, fs/shell, memtest, net/*, task/*) and C tests (helloworld, httpclient, memtest, pthread/*)
- `test-suit/starryos/normal/` -- StarryOS normal tests: board-orangepi-5-plus, qemu-aarch64-plat-dyn, qemu-dhcp, qemu-smp1, qemu-smp4, test-session-syscalls, test-time-syscalls
- `test-suit/starryos/stress/` -- Stress tests: postgresql, stress-ng-0
- `test-suit/axvisor/` -- Axvisor tests: normal, svm, board-orangepi-5-plus, qemu-aarch64-plat-dyn

**Std Test Whitelist (`scripts/test/std_crates.csv`):**
- 53 crates configured for `cargo xtask test` (std-compatible crates only)
- Excludes no_std kernel/platform modules

**Clippy Whitelist (`scripts/test/clippy_crates.csv`):**
- 103 crates configured for `cargo xtask clippy`
- Sorted alphabetically; includes `package` header

**CI Test Matrix Summary:**

| System | x86_64 | AArch64 | RISC-V 64 | LoongArch 64 | Physical Board |
|--------|--------|---------|-----------|-------------|----------------|
| ArceOS | QEMU | QEMU | QEMU | QEMU | - |
| StarryOS | QEMU | QEMU | QEMU | QEMU | Orange Pi 5+ |
| Axvisor | Self-hosted + SVM | QEMU | QEMU | QEMU (LVZ) | Orange Pi 5+ |

**Stress Tests (all marked TODO in CI):**
- StarryOS: aarch64, riscv64, loongarch64, x86_64
- Only run on PRs targeting `main`

## Environment Configuration

**Required CI Environment Variables:**

| Variable | Scope | Purpose |
|----------|-------|---------|
| `CARGO_REGISTRY_TOKEN` | GitHub Secret | crates.io publishing (release-plz) |
| `GITHUB_TOKEN` | Automatic | Container registry auth, release PRs |
| `STARRY_APK_REGION` | CI matrix | APK mirror region for StarryOS rootfs (`us` for QEMU, `china` default) |
| `AXBUILD_QEMU_SYSTEM_LOONGARCH64` | Dockerfile | Path to custom QEMU-LVZ binary (only in axvisor-lvz image) |

**Docker Environment Variables:**
- `RUSTUP_HOME=/opt/rustup` -- Rust toolchain installation path
- `CARGO_HOME=/opt/cargo` -- Cargo cache and installed binaries
- `TZ=Etc/UTC` -- Container timezone
- `PATH` includes: `/opt/cargo/bin`, `/opt/qemu-<version>/bin`, all 4 musl cross-compiler bin dirs

**VS Code Debug Environment Variables (`.vscode/session.py`):**
- `TGOS_DEBUG_COMMAND` -- shell command to build and launch QEMU
- `TGOS_DEBUG_PORT` -- GDB stub port (default: 1234)
- `TGOS_DEBUG_SESSION` -- session name: `arceos`, `axvisor`, or `starry`
- `TGOS_DEBUG_STATE_DIR` -- per-session state files directory (`target/qemu-debug`)
- `TGOS_DEBUG_TEE_OUTPUT` -- mirror QEMU stdout to terminal (default: 1)

**System Configuration Files:**
- `.arceos.toml` -- ArceOS default: `arch = "x86_64"`, `target = "x86_64-unknown-none"`, `package = "arceos-lockdep"`
- `.starry.toml` -- StarryOS default: `arch = "x86_64"`, `target = "x86_64-unknown-none"`
- `.axconfig.toml` -- Generated/default axconfig for local builds
- `.cargo/config.toml` -- Cargo aliases (`arceos`, `starry`, `axvisor`, `xtask`, `board`), git-fetch-with-cli, incompatible-rust-versions allow

## Webhooks & Callbacks

**Incoming:** None detected (no webhook receivers in this repository).

**Outgoing:**
- GitHub Container Registry push (container-publish workflow)
- GitHub Pages deployment (docs workflow)
- crates.io crate publishing (release-plz workflow)
- APK package downloads (StarryOS rootfs build -- from Alpine mirrors)
- Toolchain downloads (musl cross-compilers from GitHub Releases / musl.cc)
- QEMU source downloads (qemu.org / GitHub)

## Release Management

**Automated via release-plz:**
- On push to `main` in `rcore-os/tgoskits`: `release-plz release-pr` creates/updates a release PR
- On push to `dev` in `rcore-os/tgoskits` (after all CI checks pass): `release-plz release` publishes to crates.io
- Config: `release-plz.toml` (`[workspace] publish_no_verify = true`)
- Crate version scheme: individual versioning (each crate has its own version, e.g., `ax-hal = "0.5.12"`, `ax-mm = "0.5.12"`)

---

*Integration audit: 2026-05-13*
