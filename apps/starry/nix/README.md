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
