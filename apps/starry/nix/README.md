# Starry Nix App

This case runs a minimal Nix smoke test inside StarryOS through the app runner.
Nix is installed from the Alpine community repository during the host-side
prebuild/rootfs injection step and then run in single-user mode inside the
guest. The guest verifies the prebuilt Nix binary, evaluates a Nix expression,
and verifies a store-path write, covering the install→startup→evaluate→artifact
chain without a guest-time apk fetch.

```bash
cargo xtask starry app qemu -t nix --arch x86_64
cargo xtask starry app qemu -t nix --arch aarch64
```

## Nixpkgs Package-Manager Phase (default path)

The nix app test runs **three** phases on every invocation:

1. `nix-nosandbox` — `builtins.toFile` store-path write (CI gate since 002)
2. `nix-sandbox` — runs a minimal local derivation with substitution disabled
   and verifies that Nix kept sandbox isolation enabled
3. `nix-nixpkgs` — imports pinned nixpkgs, evaluates its `hello` derivation,
   realizes it, and runs `hello` (activated by 003)

The nosandbox and nixpkgs phases run with sandboxing disabled:
`nix-nixpkgs.sh` passes `--option sandbox false`, while `nix-nosandbox.sh` uses
the `sandbox = false` setting in `nix.conf`. The nixpkgs phase tests nixpkgs as
a package manager: it imports `/opt/nixpkgs`, evaluates a package derivation,
realizes it with binary substitution allowed, and executes the resulting program.
In the default path, Nix may substitute both `stdenv` and `hello` from
`cache.nixos.org`; it does not claim that `hello` is compiled locally.

### Nix version and source

The default path tests the **Alpine-packaged Nix** installed by `prebuild.sh`
with the target rootfs apk before StarryOS boots. Alpine v3.23 community
provides `nix 2.31.5-r0`. The Nix package manager itself may then evaluate
expressions and import nixpkgs at its own version, independent of the
apk-provided binary. This app path does **not** cover installation via the
upstream Nix binary installer from `releases.nixos.org`.

### Local source builds are optional

The default CI path intentionally does not use `--no-substitute`. Downloading
or building the nixpkgs `stdenv` closure before a local source build is slow,
so the standard test uses the prebuilt binary-cache closure first. Local source
builds remain supported when no matching substitute is available, but they are
an optional capability rather than a required CI assertion.

### Sandbox configuration

The host prebuild generates `sandbox = false` in `nix.conf`. The app scripts set
`NIX_REMOTE=local` so store operations use the guest rootfs directly instead of
expecting a daemon socket. The `nix-sandbox` phase changes the sandbox setting
to `sandbox = true`, runs `nix-build --no-substitute --option sandbox true`, and
rejects logs that report sandbox auto-disable. The later nixpkgs phase passes an
explicit `--option sandbox false`.

## Test Content

| Script | Mode | Runs? |
|--------|------|-------|
| `nix-nosandbox` | `builtins.toFile` store-path write (no builder) | ✅ CI |
| `nix-nixpkgs` | nixpkgs import/evaluation, substitution-allowed realization, and `hello` execution | ✅ CI (003) |
| `nix-sandbox` | minimal local derivation with `--no-substitute --option sandbox true` | ✅ CI |

`prebuild.sh` installs Nix into the app rootfs before boot. `test_nix.sh` then
verifies `nix --version`, runs `nix-nosandbox`, `nix-sandbox`, then
`nix-nixpkgs`. On x86_64 the sandbox script first runs the same builder with
`sandbox = false` to separate general builder failures from sandbox-specific
failures.

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

Binary substitution is allowed by default (FR-003). This makes the standard
test reproducible without requiring a slow local `stdenv` download or source
build; it does not prove that a local `buildPhase` ran.

- Install Nix during prebuild/rootfs injection → `nix --version` gate → store-path write via `builtins.toFile`
- nixpkgs: import `/opt/nixpkgs` → evaluate `hello` → realize it with substitution allowed → verify `hello` output
- Build log `.lock` / `.drv` files exercise the rsext4 open-unlink lifecycle

## Kernel Regression Tests

Kernel-level semantics (pipe poll, pidfd, rsext4 open-unlink, mount namespace
isolation, etc.) are covered separately by the unified **qemu/system** grouped
suite. The legacy SMP-specific split has been removed from the active runner
layout; the system configs now run with `-smp 4`.

```bash
cargo xtask starry test qemu --arch x86_64 -c qemu/system
cargo xtask starry test qemu --arch aarch64 -c qemu/system
```

These C-language regression tests were migrated from the former
`test-nix-prereqs` case and now live alongside other kernel regression tests
under the unified system grouped suite.
See `test-suit/starryos/qemu/system/`.

## Nix Sandbox Debug Regression Suite (003)

The `qemu/nix-sandbox-debug` grouped suite (`test-suit/starryos/qemu/nix-sandbox-debug/`)
is a CI-tracked regression suite added by the 003-starryos-nixpkgs feature.
It contains eleven focused C tests, one per Linux-semantics blocker fixed in
003, plus integration coverage for `pivot_root`. The suite runs under
`sandbox=off` (it does **not** exercise the nix sandbox builder; it only
verifies kernel semantics that the nix sandbox path depends on).

```bash
cargo xtask starry test qemu --arch x86_64 -c qemu/nix-sandbox-debug
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
| `test-pivot-root` | resolved-location `pivot_root(".", "old")` and old-root detach | `TEST_PIVOT_ROOT_PASSED` |
| `test-pivot-root-namespace` | private mount-namespace pivot does not change the parent namespace | `TEST_PIVOT_ROOT_NAMESPACE_PASSED` |
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
├── prebuild.sh                  # generate nix.conf and inject pinned nixpkgs source
├── nix.sh                       # sandbox-enabled local derivation build
├── nix-nosandbox.sh             # builtins.toFile (CI gate, ~30s)
├── nix-nixpkgs.sh               # nixpkgs realization and hello execution (CI gate)
├── test_nix.sh                  # prebuilt nix + nosandbox + sandbox + nixpkgs phases
├── build-x86_64-unknown-none.toml
├── build-aarch64-unknown-none-softfloat.toml
├── qemu-x86_64.toml             # 1800s timeout, shell_init_cmd=test_nix.sh
├── qemu-x86_64-shell.toml       # interactive shell
├── qemu-aarch64.toml
└── README.md                    # this file
```

## Dependencies

- Nix installed into the app rootfs during prebuild with Alpine v3.23 community
  `nix 2.31.5-r0`
- Host network access to GitHub during prebuild (nixpkgs source archive)
- Guest network access to `cache.nixos.org` for substitution-allowed nixpkgs
  derivations (FR-003)
