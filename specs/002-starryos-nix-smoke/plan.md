# Implementation Plan: StarryOS Nix Smoke — PR Review Fixes

**Branch**: `002-starryos-nix-smoke` | **Date**: 2026-06-08 | **Spec**: [spec.md](spec.md)

**Input**: PR #1125 review feedback from `@ZR233` (review ID 4446698115) + user-specified fix priorities.

## Summary

Address 4 blocking issues identified in the latest PR #1125 review:

1. **`unshare(CLONE_FS)`** currently mutates the shared `Arc<Mutex<FsContext>>` inner value instead of rebinding the task-local to a private copy — doesn't match Linux semantics.
2. **`test-unshare-fs`** uses `fork()` which doesn't share `FS_CONTEXT` in this kernel, so the test can't verify the fix even if it passes.
3. **`sh: mkdir: not found`** in builder script — `nix-nosandbox.sh` builder uses bare `mkdir` without absolute path, and uses `;` instead of `&&` so failure doesn't propagate.
4. **README/PR description** claims nixpkgs coverage that `test_nix.sh` intentionally skips — needs to honestly state nixpkgs is not planned at this stage.

## Technical Context

**Language/Version**: Rust nightly-2026-05-28, C (musl-gcc for user-space tests)

**Primary Dependencies**: StarryOS kernel (`os/StarryOS/kernel`), axfs-ng-vfs (`components/axfs-ng-vfs`), ax-task (`os/arceos/modules/axtask`), scope-local (`components/scope-local`)

**Storage**: rsext4 (`components/rsext4`), in-memory tmpfs

**Testing**: `cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs` for grouped regression, `cargo xtask starry app qemu -t nix --arch x86_64` for app-level validation

**Target Platform**: StarryOS x86_64 QEMU (Alpine rootfs with BusyBox)

**Project Type**: OS kernel + system-level test suite

**Constraints**:
- `CLONE_FS` unshare must match Linux: caller gets private `fs_struct`, changes don't affect sharer
- Builder args must fail-fast on error (use `&&` not `;`)
- README/PR must not claim coverage that scripts intentionally skip
- All changes must pass `cargo fmt`, `cargo xtask clippy --package starry-kernel`

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Evidence |
|-----------|--------|----------|
| I. Code Style | ✅ N/A | All edits are to .rs/.c/.sh files; `cargo fmt` and `cargo xtask clippy` must pass before commit |
| II. Strict Lint | ✅ MUST VERIFY | `cargo xtask clippy --package starry-kernel` (13 checks) must pass after namespace.rs change |
| III. Unified Build/Test | ✅ MUST VERIFY | CI-like Podman path: `cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs`, `cargo xtask starry app qemu -t nix --arch x86_64` |
| IV. Driver Architecture | ✅ N/A | No driver changes |
| V. PR Standards | ✅ MUST FOLLOW | PR body in Chinese, title in English Convention Commits |
| VI. Code Ownership | ✅ N/A | No ownership boundary changes |

**Validation commands**:
```bash
# Container path (preferred)
env -u LD_PRELOAD podman run --rm --userns=keep-id \
  -v "$PWD:/workspace" -v "$PWD/.ci-cache/cargo:/tmp/cargo" \
  -v "$PWD/.ci-cache/rustup:/tmp/rustup" -v "$PWD/.ci-cache/tmp:/tmp/ci-tmp" \
  -w /workspace -e CARGO_HOME=/tmp/cargo -e RUSTUP_HOME=/tmp/rustup -e TMPDIR=/tmp/ci-tmp \
  ghcr.io/rcore-os/tgoskits-container:latest \
  bash -lc 'cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs'
```

## Project Structure

### Documentation (this feature)

```text
specs/002-starryos-nix-smoke/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
└── tasks.md             # Phase 2 output (speckit-tasks)
```

### Source Code (affected files)

```text
os/StarryOS/kernel/src/syscall/task/namespace.rs    # unshare(CLONE_FS) fix
test-suit/starryos/qemu-smp1/test-nix-prereqs/
  test-unshare-fs/c/src/main.c                       # Rewrite: fork→clone(CLONE_FS)
apps/starry/nix/
  nix-nosandbox.sh                                   # Fix builder args (mkdir + fail-fast)
  test_nix.sh                                        # Clarify nixpkgs skip reason
  README.md                                          # Align with actual test coverage
```

## Complexity Tracking

No constitution violations. All fixes are minimal, targeted changes within existing patterns.

---

## Phase 0: Research

### R1: Linux `unshare(CLONE_FS)` semantics

**Decision**: Linux `unshare(CLONE_FS)` requires the caller to obtain a private `fs_struct`. After `clone(CLONE_FS)`, parent and child share the same `struct fs_struct` (refcounted). `unshare(CLONE_FS)` creates a new private copy for the caller, breaking the share.

**Rationale**: Per `man 2 unshare`: "CLONE_FS — Reverse the effect of the clone(2) CLONE_FS flag. Unshare the filesystem attributes so that the calling process has a private copy of its filesystem information." The current implementation clones the inner `FsContext` but keeps the shared `Arc`, which means both tasks still observe each other's `chdir`/`chroot`/mount changes.

**Alternatives considered**:
- Keep cloning inner `FsContext` inside shared `Arc` — REJECTED, doesn't match Linux behavior
- Mark `FS_CONTEXT` as dirty and lazily re-clone on next access — REJECTED, over-engineered for current scope

**Implementation**: Use `FS_CONTEXT.scope_mut(&mut scope)` to rebind the task-local to a new `Arc::new(Mutex::new(cloned_inner))`.

**Files**: `os/StarryOS/kernel/src/syscall/task/namespace.rs` lines 59-65

### R2: `test-unshare-fs` test gap

**Decision**: Rewrite the test to use `clone(CLONE_FS | CLONE_VM | SIGCHLD)` with a heap-allocated stack, then verify: (a) after clone with CLONE_FS, child's `chdir` is visible to parent (proving FS sharing), (b) after child calls `unshare(CLONE_FS)`, child's `chdir` is NOT visible to parent (proving isolation).

**Rationale**: The current test uses `fork()` which in this StarryOS implementation creates a new `FsContext` clone (see `clone.rs` line 338), so cwd is already isolated. The test passes but proves nothing about `CLONE_FS` sharing/unshare.

**Implementation**: Full rewrite of `test-unshare-fs/c/src/main.c`.

### R3: `sh: mkdir: not found` in builder

**Decision**: In `nix-nosandbox.sh`, change the derivation builder args from `;` to `&&` for fail-fast, and use absolute paths for shell utilities.

**Rationale**: Nix's derivation builder runs `/bin/sh -c` without a guaranteed PATH. BusyBox's `mkdir` may not be found if PATH isn't set. Using `&&` ensures any step failure propagates. Since the guest rootfs is Alpine with BusyBox, all basic utilities live under `/bin/`.

**Current problem**:
```sh
args = [ "-c" "mkdir -p /tmp/nix-nosandbox; echo BUILDER_STARTED > ..." ];
```
`mkdir` fails → `sh: mkdir: not found` → but `;` continues → `echo BUILDER_STARTED` runs → false PASS.

**Fix**:
```sh
args = [ "-c" "/bin/mkdir -p /tmp/nix-nosandbox && echo BUILDER_STARTED > ..." ];
```
Now `mkdir` failure → `&&` stops → build fails → correctly classified as failure.

### R4: README/PR nixpkgs documentation

**Decision**: Update `apps/starry/nix/README.md` to clearly state:
- `test_nix.sh` intentionally skips `nix-nixpkgs` (not just commented out for debugging)
- nixpkgs/stdenv.mkDerivation testing is not planned at this stage per project discussion
- The reason: requires mount namespace isolation for fetchTarball/substitute downloads
- Update the test content table to show accurate status

Also update PR #1125 body to match.

**Implementation**: Edit `apps/starry/nix/README.md` and update PR body via `gh pr edit`.

---

## Phase 1: Design & Contracts

### Data Model

No new entities. Changes affect existing structures:

- `FsContext`: existing struct in `components/axfs-ng-vfs/src/mount.rs`. Has `clone()` via `#[derive(Clone)]`.
- `FS_CONTEXT`: existing task-local `Arc<Mutex<FsContext>>` in `os/arceos/modules/axfs-ng/src/highlevel/fs.rs`.
- `NsProxy`: existing struct in `os/StarryOS/axnsproxy/`. No changes needed.

State transition for `unshare(CLONE_FS)`:
```
Before: task.FS_CONTEXT == parent.FS_CONTEXT (same Arc, refcount >= 2)
  ↓ unshare(CLONE_FS)
After:  task.FS_CONTEXT == new Arc(Mutex(clone(inner)))
        parent.FS_CONTEXT unchanged
```

### Contracts

**`sys_unshare(CLONE_FS)` behavior contract**:
- **Precondition**: CLONE_FS flag is set, CLONE_NEWNS is not set
- **Postcondition**: Caller's FS_CONTEXT is a new private `Arc<Mutex<FsContext>>` with cloned inner state. Cwd/root/umask match pre-call values but subsequent changes don't affect tasks that shared the original Arc.
- **Error**: Returns 0 on success.

**`test_nix.sh` contract update**:
- **Current behavior**: Runs `nix-nosandbox` only; `nix-nixpkgs` is explicitly skipped.
- **Exit code**: 0 if nosandbox passes; non-zero if nosandbox fails.
- **Result marker**: `NIX_ALL_TESTS_PASSED` printed after nosandbox success only.

### Quickstart

**To verify `unshare(CLONE_FS)` fix**:
```bash
cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs
# Look for: UNSHARE_FS_CLONE_ISOLATION_PASSED
```

**To verify builder fix**:
```bash
cargo xtask starry app qemu -t nix --arch x86_64
# Look for: NIX_LOCAL_BUILD_NOSANDBOX_OK (no "sh: mkdir: not found")
```

**To run in CI-like container**:
```bash
env -u LD_PRELOAD podman run --rm --userns=keep-id \
  -v "$PWD:/workspace" -v "$PWD/.ci-cache/cargo:/tmp/cargo" \
  -v "$PWD/.ci-cache/rustup:/tmp/rustup" -v "$PWD/.ci-cache/tmp:/tmp/ci-tmp" \
  -w /workspace -e CARGO_HOME=/tmp/cargo -e RUSTUP_HOME=/tmp/rustup -e TMPDIR=/tmp/ci-tmp \
  ghcr.io/rcore-os/tgoskits-container:latest \
  bash -lc 'cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs'
```
