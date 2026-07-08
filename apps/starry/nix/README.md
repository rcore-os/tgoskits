# Starry Nix App

This case runs a minimal Nix smoke test inside StarryOS through the app runner.
A prebuilt Nix is injected into the Alpine rootfs; the guest evaluates a Nix
expression and verifies a store-path write, covering the
install→startup→evaluate→artifact chain.

```bash
cargo xtask starry app qemu -t nix --arch x86_64
cargo xtask starry app qemu -t nix --arch aarch64
```

## Nixpkgs Phase Activated (default path)

The nix app test runs **two** phases on every invocation:

1. `nix-nosandbox` — `builtins.toFile` store-path write (CI gate since 002)
2. `nix-nixpkgs` — `nixpkgs.stdenv.mkDerivation` source build of a minimal C
   `hello` program (activated by 003)

Both phases use `--option sandbox false` (see `nix-nixpkgs.sh` and
`nix-nosandbox.sh`). The default nixpkgs path allows binary cache
(`cache.nixos.org`) for stdenv build inputs so the test completes in a
reasonable CI window; the target derivation's `buildPhase` (the `hello.c` →
`hello` compile step) still runs locally on StarryOS, exercising nixpkgs
evaluation, the builder protocol, and stdenv usage.

### Why `sandbox = false` is the default

StarryOS now supports all seven namespace flags including `CLONE_NEWNS`
(landed via PR #981), so the build sandbox path is no longer blocked at the
kernel level. The default path keeps `sandbox = false` by design (research.md
R3/R4): the default nixpkgs workflow (binary cache + local `buildPhase`)
does not need the sandbox, and keeping it off avoids pulling the full sandbox
closure. The sandboxed variant (`nix.sh`, FR-017) remains an optional stretch
target.

### Sandboxed builds (optional, not yet CI)

`nix.sh` (`nix-build --option sandbox true`) is still not in the CI gate. It
will be connected once the sandboxed build path is validated end-to-end on
both architectures. Running it today would produce a **false PASS** if the
sandbox were silently auto-disabled, so it is intentionally excluded from
`test_nix.sh` until the sandbox path is explicitly validated.

## Test Content

| Script | Mode | Runs? |
|--------|------|-------|
| `nix-nosandbox` | `builtins.toFile` store-path write (no builder) | ✅ CI |
| `nix-nixpkgs` | `pkgs.stdenv.mkDerivation` source build (binary cache allowed) | ✅ CI (003) |
| `nix` | `nix-build --option sandbox true` (full sandboxed derivation builder) | ❌ optional (FR-017, not yet CI) |

`test_nix.sh` runs `nix-nosandbox` then `nix-nixpkgs`. The sandbox test
(`nix.sh`) is intentionally not injected until the sandboxed path is validated.

### nixpkgs pin

The host prebuild downloads an immutable nixpkgs commit archive, verifies its
SHA256, and injects the extracted source tree at `/opt/nixpkgs`. The guest
imports that path directly. This avoids Nix's Git-cache tarball import, whose
metadata-heavy writes can exhaust the StarryOS test window before evaluation
starts.

- Commit: `714a5f8c4ead6b31148d829288440ed033ccc041` (`release-26.05`)
- Archive SHA256: `96009df77ed2339619ddc93fd99e7a2aeea13299bc5e0620314b6e475e015b36`
- Pin location: `prebuild.sh`

To update the pin, download the new immutable commit archive and update both
the commit and SHA256 in `prebuild.sh`:

```bash
curl -L -o nixpkgs.tar.gz https://github.com/NixOS/nixpkgs/archive/<commit>.tar.gz
sha256sum nixpkgs.tar.gz
```

Binary cache for stdenv inputs is allowed by default (FR-003); only the
target derivation's `buildPhase` (the `hello.c` → `hello` compile step)
runs locally on StarryOS.

- Install the official Nix closure → `nix --version` gate → store-path write via `builtins.toFile`
- nixpkgs: import `/opt/nixpkgs` → evaluate `stdenv.mkDerivation` → `nix-build` → verify `hello` output
- Build log `.lock` / `.drv` files exercise the rsext4 open-unlink lifecycle

## Kernel Regression Tests

Kernel-level semantics (pipe poll, pidfd, rsext4 open-unlink, mount namespace
isolation, etc.) are covered separately by the **qemu-smp1/system** grouped suite:

```bash
cargo xtask starry test qemu --arch x86_64 -c qemu-smp1/system
cargo xtask starry test qemu --arch aarch64 -c qemu-smp1/system
```

These C-language regression tests were migrated from the former
`test-nix-prereqs` case and now live alongside other kernel regression tests
under the unified system grouped suite.
See `test-suit/starryos/qemu-smp1/system/`.

## Nix Sandbox Debug Regression Suite (003)

The `qemu-smp1/nix-sandbox-debug` grouped suite (`test-suit/starryos/qemu-smp1/nix-sandbox-debug/`)
is a CI-tracked regression suite added by the 003-starryos-nixpkgs feature.
It contains ten focused C tests, one per Linux-semantics blocker fixed in
003, plus integration coverage for `pivot_root`. The suite runs under
`sandbox=off` (it does **not** exercise the nix sandbox builder; it only
verifies kernel semantics that the nix sandbox path depends on).

```bash
cargo xtask starry test qemu --arch x86_64 -c qemu-smp1/nix-sandbox-debug
```

The runner emits `NIX_SANDBOX_DEBUG_TESTS_PASSED` on success. Each test
binary lives under `/usr/bin/starry-test-suit/` in the guest rootfs and
prints its own `<NAME>_PASSED` marker.

### Covered semantics

| Test | Kernel area | Marker |
|------|-------------|--------|
| `test-mountinfo` | `/proc/<pid>/mountinfo` Linux-compatible format and dynamic mounts | `TEST_MOUNTINFO_PASSED` |
| `test-per-ns-mounts` | Per-mount-namespace mount visibility (`CLONE_NEWNS`) | `TEST_PER_NS_MOUNTS_PASSED` |
| `test-remount-flags` | `mount(MS_REMOUNT|MS_NOSUID)` reflected in mountinfo options | `TEST_REMOUNT_FLAGS_PASSED` |
| `test-mount-bind` | `mount --bind` and `--rbind` semantics | `TEST_MOUNT_BIND_PASSED` |
| `test-mount-propagation` | Shared peer group unmount propagation | `TEST_MOUNT_PROPAGATION_PASSED` |
| `test-pivot-root` | `pivot_root` workaround path with absolute `new_root`/`put_old` | `TEST_PIVOT_ROOT_PASSED` |
| `test-cgroup-ns` | `unshare`/`clone`/`setns` for `CLONE_NEWCGROUP` + `/proc/self/ns/cgroup` | `TEST_CGROUP_NS_PASSED` |
| `test-max-ns-entries` | All seven `/proc/sys/user/max_*_namespaces` files | `TEST_MAX_NS_ENTRIES_PASSED` |
| `test-proc-environ` | `/proc/<pid>/environ` NUL-separated envp | `TEST_PROC_ENVIRON_PASSED` |
| `test-proc-root-cwd` | `/proc/<pid>/root` and `/proc/<pid>/cwd` symlinks track `chdir`/`chroot` | `TEST_PROC_ROOT_CWD_PASSED` |

### Why `pivot-root` runs last

`pivot_root` in StarryOS mirrors Linux `chroot_fs_refs()`: every task whose
root or cwd matched the old root is repointed at the new root. The
`test-pivot-root` case therefore leaves the runner shell inside the new root
once it exits, so the suite runner places it last to avoid breaking
subsequent test binaries. Cosmetic `can't create /dev/null` messages may
appear after the `NIX_SANDBOX_DEBUG_TESTS_PASSED` marker; they are expected
post-test noise and are not treated as failures by the runner's
success/fail regexes.

## File Structure

```
apps/starry/nix/
├── prebuild.sh          # inject official Nix and pinned nixpkgs source
├── nix.sh               # sandbox-enabled nix-build (blocked, not CI)
├── nix-nosandbox.sh     # builtins.derivation (CI gate, ~30s)
├── nix-nixpkgs.sh       # stdenv.mkDerivation (CI gate, binary cache allowed)
├── test_nix.sh          # nosandbox + nixpkgs phases
├── build-x86_64-unknown-none.toml
├── build-aarch64-unknown-none-softfloat.toml
├── qemu-x86_64.toml     # 1200s timeout, shell_init_cmd=test_nix.sh
├── qemu-aarch64.toml
└── README.md            # this file
```

## Dependencies

- Nix 2.34.0 official binary closure, architecture-pinned in `prebuild.sh`
- Host network access to GitHub during prebuild and guest access to
  `cache.nixos.org` for stdenv inputs (allowed by default per FR-003)
