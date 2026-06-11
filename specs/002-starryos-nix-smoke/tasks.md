# Tasks: StarryOS Nix Smoke — PR Review Fixes

**Input**: Design documents from `/specs/002-starryos-nix-smoke/`

**Prerequisites**: plan.md ✅, spec.md ✅, research.md ✅, data-model.md ✅, contracts/ ✅, quickstart.md ✅

**Branch**: `002-starryos-nix-smoke` | **PR**: #1125 review `@ZR233` (ID 4446698115)

**Organization**: Tasks are grouped by fix area. All 4 fix areas are independent and can proceed in parallel.

## Format: `[ID] [P?] [Story?] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Verify current state and establish baseline

- [x] T001 Verify current branch `002-starryos-nix-smoke` is clean with `git status` and `git log --oneline -5`
- [x] T002 [P] Run `cargo xtask clippy --package starry-kernel` to establish baseline (expected: 13/13 PASS)
- [x] T003 [P] Run `cargo fmt --check` to confirm no formatting drift before changes

---

## Phase 2: Fix 1 — `unshare(CLONE_FS)` Kernel Fix (US3)

**Goal**: Fix `unshare(CLONE_FS)` to rebind task-local `FS_CONTEXT` to a new `Arc<Mutex<FsContext>>` instead of mutating the shared Arc inner value, matching Linux semantics.

**Independent Test**: Run `cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs` and verify `UNSHARE_FS_CLONE_ISOLATION_PASSED` appears.

**Source**: plan.md R1, research.md R1, data-model.md, contracts/fix-unshare-fs.md

### Implementation for Fix 1

- [x] T004 [US3] Rewrite `unshare(CLONE_FS)` in `os/StarryOS/kernel/src/syscall/task/namespace.rs` to use `FS_CONTEXT.scope_mut(&mut scope)` to rebind to new `Arc::new(Mutex::new(cloned_inner))` instead of mutating shared Arc inner
- [x] T005 [US3] Run `cargo fmt` on `os/StarryOS/kernel/src/syscall/task/namespace.rs`
- [x] T006 [US3] Run `cargo xtask clippy --package starry-kernel` and verify 13/13 PASS
- [x] T007 [US3] Build StarryOS kernel with `cargo xtask starry build --arch x86_64` to confirm compilation

**Checkpoint**: Kernel compiles cleanly with the `unshare(CLONE_FS)` fix.

---

## Phase 3: Fix 2 — `test-unshare-fs` Test Rewrite (US3)

**Goal**: Rewrite `test-unshare-fs` to use `clone(CLONE_FS | CLONE_VM | SIGCHLD)` instead of `fork()`, verifying that (a) after clone with CLONE_FS, child's `chdir` is visible to parent (FS sharing), and (b) after child calls `unshare(CLONE_FS)`, child's `chdir` is NOT visible to parent (isolation).

**Independent Test**: Run `cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs` and verify both `UNSHARE_FS_BASIC_PASSED` and `UNSHARE_FS_CLONE_ISOLATION_PASSED` appear.

**Source**: plan.md R2, research.md R2, contracts/focused-regression.md

### Implementation for Fix 2

- [x] T008 [US3] Rewrite `test-suit/starryos/qemu-smp1/test-nix-prereqs/test-unshare-fs/c/src/main.c` to use `clone(CLONE_FS | CLONE_VM | SIGCHLD)` with heap-allocated stack, testing shared-cwd phase then isolated-cwd phase
- [x] T009 [US3] Verify test by running `cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs` and checking for `UNSHARE_FS_CLONE_ISOLATION_PASSED` in output

**Checkpoint**: Test correctly validates `unshare(CLONE_FS)` isolation behavior with `clone(CLONE_FS)`.

---

## Phase 4: Fix 3 — `nix-nosandbox.sh` Builder Fix (US2)

**Goal**: Fix the Nix derivation builder in `nix-nosandbox.sh` to use absolute paths (`/bin/mkdir`) and fail-fast operators (`&&` instead of `;`) so that build failures propagate correctly.

**Independent Test**: Run `cargo xtask starry app qemu -t nix --arch x86_64` and verify `NIX_LOCAL_BUILD_NOSANDBOX_OK` appears without `sh: mkdir: not found`.

**Source**: plan.md R3, research.md R3, data-model.md, contracts/fix-builder-mkdir.md

### Implementation for Fix 3

- [x] T010 [US2] Fix builder args in `apps/starry/nix/nix-nosandbox.sh`: change `mkdir -p ...; echo BUILDER_STARTED` to `/bin/mkdir -p ... && echo BUILDER_STARTED`, and apply `&&` to all subsequent builder commands
- [x] T011 [US2] Verify fix by running `cargo xtask starry app qemu -t nix --arch x86_64` and checking for `NIX_LOCAL_BUILD_NOSANDBOX_OK` without `sh: mkdir: not found`

**Checkpoint**: Builder fails fast on errors, no false PASS from `mkdir` failure.

---

## Phase 5: Fix 4 — README/PR nixpkgs Documentation (US5)

**Goal**: Update `apps/starry/nix/README.md` to honestly state that nixpkgs/stdenv.mkDerivation testing is not planned at this stage, with clear reasoning. Update PR #1125 body to match.

**Independent Test**: Read `apps/starry/nix/README.md` and confirm it clearly states nixpkgs is deferred, with the reason (requires mount namespace isolation for fetchTarball/substitute downloads).

**Source**: plan.md R4, research.md R4, contracts/doc-nixpkgs-deferral.md

### Implementation for Fix 4

- [x] T012 [US5] Update `apps/starry/nix/README.md` to state nixpkgs testing is "not planned at this stage", explain the reason (requires mount namespace isolation), and fix the test content table to show accurate status
- [x] T013 [US5] Update PR #1125 body via `gh pr edit 1125 --body "..."` to align with the README changes (Chinese body per project convention)

**Checkpoint**: README and PR body accurately reflect current scope — no misleading nixpkgs coverage claims.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Final validation, regression check, and cleanup

- [x] T014 Run `cargo fmt` on all changed files and verify no formatting issues
- [x] T015 Run `cargo xtask clippy --package starry-kernel` and verify 13/13 PASS
- [x] T016 Run full grouped regression: `cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs` and verify all expected markers appear
- [x] T017 Run app-level validation: `cargo xtask starry app qemu -t nix --arch x86_64` and verify `NIX_LOCAL_BUILD_NOSANDBOX_OK`
- [x] T018 Run `git diff --stat` to review all changed files match the expected set: `namespace.rs`, `test-unshare-fs/c/src/main.c`, `nix-nosandbox.sh`, `README.md`

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately
- **Fix 1 (Phase 2)**: No dependencies on other fixes — can start immediately
- **Fix 2 (Phase 3)**: Depends on Fix 1 (Phase 2) — test must run against fixed kernel
- **Fix 3 (Phase 4)**: No dependencies on other fixes — can start immediately
- **Fix 4 (Phase 5)**: No dependencies on other fixes — can start immediately
- **Polish (Phase 6)**: Depends on all fixes being complete

### Fix Area Dependencies

```
Phase 1: Setup
    │
    ├── Phase 2: Fix 1 (unshare CLONE_FS) ──→ Phase 3: Fix 2 (test rewrite)
    │                                              │
    ├── Phase 4: Fix 3 (builder mkdir) ────────────┤
    │                                              │
    └── Phase 5: Fix 4 (README nixpkgs) ───────────┤
                                                   │
                                          Phase 6: Polish
```

### Parallel Opportunities

- **Phase 1**: T002 and T003 can run in parallel
- **Phase 2 + Phase 4 + Phase 5**: Fix 1, Fix 3, and Fix 4 are independent and can proceed in parallel
- **Phase 3**: Must wait for Phase 2 (Fix 1) to complete before running the rewritten test
- **Phase 6**: T014, T015 can run in parallel; T016, T017 can run in parallel after

---

## Parallel Example: Fix 1 + Fix 3 + Fix 4

```bash
# These three fix areas are independent — launch in parallel:
Task: "Rewrite unshare(CLONE_FS) in namespace.rs" (Fix 1)
Task: "Fix builder args in nix-nosandbox.sh" (Fix 3)
Task: "Update README.md nixpkgs documentation" (Fix 4)
```

---

## Implementation Strategy

### MVP First (Fix 1 + Fix 2)

1. Complete Phase 1: Setup
2. Complete Phase 2: Fix 1 (unshare CLONE_FS kernel fix)
3. Complete Phase 3: Fix 2 (test rewrite)
4. **STOP and VALIDATE**: Run `cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs`
5. Verify `UNSHARE_FS_CLONE_ISOLATION_PASSED`

### Incremental Delivery

1. Complete Setup → baseline established
2. Add Fix 1 + Fix 2 → kernel fix + test → validate
3. Add Fix 3 → builder fix → validate app-level
4. Add Fix 4 → documentation → review
5. Polish → full regression + clippy + fmt

### Single Developer Strategy

1. Complete Setup (Phase 1)
2. Fix 1 (Phase 2) → Fix 2 (Phase 3) sequentially (dependency chain)
3. Fix 3 (Phase 4) and Fix 4 (Phase 5) in any order
4. Polish (Phase 6) last

---

## Notes

- [P] tasks = different files, no dependencies
- Fix 1 and Fix 2 form a dependency chain (kernel fix → test rewrite)
- Fix 3 and Fix 4 are fully independent of Fix 1/2
- All validation commands use `cargo xtask` per project convention
- Container-based CI validation uses `podman` with `.ci-cache/` mounts
- PR body must be in Chinese per project convention
- All changes must pass `cargo fmt` and `cargo xtask clippy --package starry-kernel`
