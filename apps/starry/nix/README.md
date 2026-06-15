# Starry Nix App

This case runs a minimal Nix smoke test inside StarryOS through the app runner.
A prebuilt Nix is injected into the Alpine rootfs; the guest runs a tiny local
derivation that writes `NIX_LOCAL_BUILD_OK` to the output, covering the
install→startup→build→artifact chain.

```bash
cargo xtask starry app qemu -t nix --arch x86_64
cargo xtask starry app qemu -t nix --arch aarch64
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
| `nix-nixpkgs` | `pkgs.stdenv.mkDerivation` (requires nixpkgs) | ❌ deferred (see below) |
| `nix` | `nix-build --option sandbox true` | ❌ blocked (mount ns) |

`test_nix.sh` runs only the `nix-nosandbox` phase. The sandbox test (`nix.sh`)
is blocked until mount namespace isolation is ready.

### nixpkgs / `stdenv.mkDerivation` — not planned at this stage

Per project discussion on PR #1125 and teacher guidance, nixpkgs testing
is deferred. `stdenv.mkDerivation` requires:

- **Mount namespace isolation** (`unshare(CLONE_NEWNS)`) for the Nix download
  subsystem, which fetches nixpkgs tarballs and substitutes during build;
- A working `builtins.fetchTarball` that can download and unpack pinned
  nixpkgs revisions from GitHub through Nix's download worker threads.

These require kernel-level namespace support that is not yet available in
StarryOS. The `nix-nixpkgs` script source is committed for reference but
is intentionally excluded from `test_nix.sh`. Re-enable when mount namespace
isolation lands.
- Install prebuilt Nix (apk) → `nix --version` gate → tiny local derivation
- Build log `.lock` / `.drv` files exercise the rsext4 open-unlink lifecycle
- Sandbox detection: `grep` build log for `disabling sandbox` → call `fail()`

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
├── prebuild.sh          # apk add nix into staging rootfs
├── nix.sh               # sandbox-enabled nix-build (blocked, not CI)
├── nix-nosandbox.sh     # builtins.derivation (CI gate, ~30s)
├── nix-nixpkgs.sh       # stdenv.mkDerivation (deferred, requires mount ns)
├── test_nix.sh          # nosandbox only (nixpkgs intentionally skipped)
├── build-x86_64-unknown-none.toml
├── build-aarch64-unknown-none-softfloat.toml
├── qemu-x86_64.toml     # 1200s timeout, shell_init_cmd=test_nix.sh
├── qemu-aarch64.toml
└── README.md            # this file
```

## Dependencies

- Nix 2.31.5 (prebuilt via `apk add nix` in `prebuild.sh`)
- Alpine musl shared libraries (libc, libcrypto, libcurl, libgit2, libseccomp,
  libsodium, libsqlite3, libssh2, etc.)
- Guest network access during prebuild only (Nix is injected into rootfs; no
  network required at QEMU runtime)
