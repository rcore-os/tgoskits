# Starry Nix App

This case runs a minimal Nix smoke test inside StarryOS through the app runner.
A prebuilt Nix is injected into the Alpine rootfs; the guest runs a tiny local
derivation that writes `NIX_LOCAL_BUILD_OK` to the output, covering the
install→startup→build→artifact chain.

```bash
cargo xtask starry app qemu -t nix --arch x86_64
```

## No-Sandbox Only

Sandboxed `nix-build` is not yet enabled. Nix requires mount namespace
isolation to activate the build sandbox, and StarryOS mount namespace support is
still incomplete. Without working `unshare(CLONE_NEWNS)`, Nix prints:

> auto-disabling sandboxing because the prerequisite namespaces are not
> available

and silently falls back to unsandboxed mode. Running the sandbox variant as-is
would produce a **false PASS** — the build succeeds, but the claimed
sandboxed-build behaviour was never exercised.

Per the [discussion](https://github.com/rcore-os/tgoskits/pull/1125#issuecomment-4639168301)
on PR #1125: the teacher advised small-step iteration and submitting
no-sandbox first. The current version exercises only `builtins.derivation`
(a basic test); `stdenv.mkDerivation`, which most Nix packages use, has not
been tested yet. Sandbox support is deferred until mount namespace isolation
is ready.

For now `test_nix.sh` intentionally skips the sandbox test and only runs
`nix-nosandbox`, which passes `--option sandbox false` explicitly so the result
honestly reflects the exercised code path. The sandbox test (`nix.sh`) will be
connected once mount namespace isolation is available in StarryOS.

## Test Content

| Script | Mode | Runs? |
|--------|------|-------|
| `nix-nosandbox` | `builtins.derivation` (no nixpkgs) | ✅ CI |
| `nix-nixpkgs` | `pkgs.stdenv.mkDerivation` (requires nixpkgs) | ✅ CI |
| `nix` | `nix-build --option sandbox true` | ❌ blocked (mount ns) |

`test_nix.sh` runs phases in order: nosandbox gate first (fast, ~30s),
then nixpkgs (requires network for nixpkgs tarball + stdenv substitutes,
~5-15min first run, ~600s timeout).

The nixpkgs test uses `builtins.fetchTarball` to fetch a pinned nixpkgs
revision from GitHub, imports it, and builds a minimal C hello-world with
`pkgs.stdenv.mkDerivation`. Substitutes are allowed (`--no-substitute` is
NOT passed) so the stdenv toolchain is downloaded from `cache.nixos.org`
rather than bootstrapped from source.

Both nosandbox variants:
- Install prebuilt Nix (apk) → `nix --version` gate → tiny local derivation
- Build log `.lock` / `.drv` files exercise the rsext4 open-unlink lifecycle
- Sandbox detection: `grep` build log for `disabling sandbox` → call `fail()`

## Kernel Regression Tests

Kernel-level semantics (pipe poll, pidfd, rsext4 open-unlink, mount namespace
isolation, etc.) are covered separately by **test-nix-prereqs**:

```bash
cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs
```

That grouped case contains focused C-language regression tests independent of
the Nix app workflow, making them reviewable and runnable across CI targets.
See `test-suit/starryos/normal/qemu-smp1/test-nix-prereqs/`.

## File Structure

```
apps/starry/nix/
├── prebuild.sh          # apk add nix into staging rootfs
├── nix.sh               # sandbox-enabled nix-build (blocked, not CI)
├── nix-nosandbox.sh     # builtins.derivation (CI gate, ~30s)
├── nix-nixpkgs.sh       # pkgs.stdenv.mkDerivation (CI, requires network)
├── test_nix.sh          # unified entry — runs nosandbox → nixpkgs in order
├── build-x86_64-unknown-none.toml
├── qemu-x86_64.toml     # 1200s timeout, shell_init_cmd=test_nix.sh
└── README.md            # this file
```

## Dependencies

- Nix 2.31.5 (prebuilt via `apk add nix` in `prebuild.sh`)
- Alpine musl shared libraries (libc, libcrypto, libcurl, libgit2, libseccomp,
  libsodium, libsqlite3, libssh2, etc.)
- Guest network access during prebuild only (Nix is injected into rootfs; no
  network required at QEMU runtime)
