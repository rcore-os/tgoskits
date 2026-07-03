# Starry Nix App

This case runs a minimal Nix smoke test inside StarryOS through the app runner.
A prebuilt Nix is injected into the Alpine rootfs; the guest evaluates a Nix
expression and verifies a store-path write, covering the
install→startup→evaluate→artifact chain.

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
no-sandbox first. The current version uses `builtins.toFile` for store-path
creation, which verifies Nix evaluation and store writes without depending on
the builder communication protocol (socketpair).  Full `builtins.derivation`
builder workflow is deferred until the builder protocol (Nix socketpair hook)
is working on StarryOS — currently blocked by a poll notification gap in the
IRQ-safe deferred notification layer.

For now `test_nix.sh` intentionally skips the sandbox test and only runs
`nix-nosandbox`, which passes `--option sandbox false` explicitly so the result
honestly reflects the exercised code path. The sandbox test (`nix.sh`) will be
connected once mount namespace isolation is available in StarryOS.

## Test Content

| Script | Mode | Runs? |
|--------|------|-------|
| `nix-nosandbox` | `builtins.toFile` store-path write (no builder) | ✅ CI |
| `nix` | `nix-build --option sandbox true` (full derivation builder) | ❌ blocked (mount ns + socketpair) |

`test_nix.sh` runs only the `nix-nosandbox` phase. The sandbox test (`nix.sh`)
is blocked until mount namespace isolation is ready.

nixpkgs / `stdenv.mkDerivation` testing is tracked on a separate branch and is
intentionally not part of this smoke test; it requires mount namespace
isolation and a working `builtins.fetchTarball` download path that StarryOS
does not yet provide.

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
├── nix-nosandbox.sh     # builtins.toFile store-path write (CI gate)
├── test_nix.sh          # nosandbox only
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
