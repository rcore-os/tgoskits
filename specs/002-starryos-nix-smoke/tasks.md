# Tasks: CLONE_FS unshare 支持 + Nix 回归测试

**Input**: Design documents from `/specs/002-starryos-nix-smoke/`

**Prerequisites**: plan.md (required), spec.md (required), research.md

**Goal**: 在 `sys_unshare` 中添加 `CLONE_FS` 支持，修复 Nix 下载子系统阻塞，并在 test-nix-prereqs 中补充语义回归测试。

**Tests**: C 回归测试 — 标记 "nixpkgs 测试可能用到"。

**Organization**: Tasks are grouped by user story per plan.md.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Maps to user story from spec.md:
  - US2 — Run Nix and capture failures (CLONE_FS 修复直接解除 Nix 下载阻塞)
  - US3 — Convert kernel gaps into focused regression tests (test-unshare-fs)
  - US4 — Preserve local investigation notes and upstream context (silicalet/)

---

## Phase 1: Setup — 基线确认

**Purpose**: 确认当前 head 干净，gate 均通过。

- [X] T001 Confirm `cargo fmt --check` passes on current head
- [X] T002 [P] Confirm `cargo xtask clippy --package starry-kernel` passes (0 warnings)
- [X] T003 [P] Confirm test-nix-prereqs 12/12 PASSED before changes via container:
  ```bash
  podman run --rm --userns=keep-id \
    -v $PWD:/workspace -w /workspace \
    -v $PWD/.ci-cache/cargo:/tmp/cargo \
    -v $PWD/.ci-cache/rustup:/tmp/rustup \
    -e CARGO_HOME=/tmp/cargo -e RUSTUP_HOME=/tmp/rustup \
    ghcr.io/rcore-os/tgoskits-container:latest \
    cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs
  ```

**Checkpoint**: 基线确认，12/12 通过。

---

## Phase 2: US2 — CLONE_FS 内核修复 (Priority: P1) 🎯 MVP

**Goal**: `sys_unshare` 接受 `CLONE_FS` (0x200)，Nix 下载子系统不再报 EINVAL。

**Independent Test**:
```bash
# Container: nixpkgs test 的 fetchTarball 下载线程不再崩溃
# 端到端验证: 见 US3 回归测试
```

### Implementation for US2

- [X] T004 [US2] Add `CLONE_FS` to `SUPPORTED_NS_FLAGS` in `os/StarryOS/kernel/src/syscall/task/namespace.rs` line 13-14
- [X] T005 [US2] Add Phase 2 `CLONE_FS` branch in `sys_unshare()` in `os/StarryOS/kernel/src/syscall/task/namespace.rs`:
  - Clone current `FsContext` via `FS_CONTEXT.lock().clone()` to break sharing
  - Place before or alongside existing `CLONE_NEWNS` branch (line 49-52)
- [X] T006 [US2] Run `cargo fmt` on `os/StarryOS/kernel/src/syscall/task/namespace.rs`
- [X] T007 [US2] Run `cargo xtask clippy --package starry-kernel` — 0 warnings required
- [X] T008 [US2] Verify nix-nosandbox still PASSES (no regression):
  ```bash
  podman run ... cargo xtask starry app qemu -t nix --arch x86_64
  # Expected: nix-nosandbox PASSED (nix-nixpkgs still expected FAIL — wait for CLONE_FS followups)
  ```

**Checkpoint**: CLONE_FS 编译通过 + fmt/clippy clean + nosandbox 无回归。

---

## Phase 3: US3 — test-unshare-fs 回归测试 (Priority: P1)

**Goal**: 在 test-nix-prereqs 中添加 C 回归测试，验证 `unshare(CLONE_FS)` 语义正确性。测试注释中标注 "nixpkgs 测试可能用到"。

**Independent Test**:
```bash
cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs
# Expected: 13/13 PASSED (新增 test-unshare-fs)
```

### Tests for US3 (write FIRST, ensure RED before CLONE_FS fix)

- [X] T009 [P] [US3] Create test directory `test-suit/starryos/normal/qemu-smp1/test-nix-prereqs/test-unshare-fs/c/`
- [X] T010 [P] [US3] Write `CMakeLists.txt` in `test-suit/starryos/normal/qemu-smp1/test-nix-prereqs/test-unshare-fs/c/CMakeLists.txt` (reference existing test-open-unlink-write pattern)
- [X] T011 [US3] Write `src/main.c` in `test-suit/starryos/normal/qemu-smp1/test-nix-prereqs/test-unshare-fs/c/src/main.c`:
  - **Scenario 1 (共享+断开)**: Parent `clone(CLONE_FS)` → child shares parent FsContext → child `unshare(CLONE_FS)` → child gets independent FsContext → both chdir different paths → verify isolation
  - **Scenario 2 (已独立=nop)**: `unshare(CLONE_FS)` on already-independent task → returns 0, cwd unchanged
  - **Scenario 3 (非法标志)**: `unshare(0xDEAD)` → returns EINVAL
  - Top comment: `nixpkgs 测试可能用到 — unshare(CLONE_FS) 是 Nix fetchTarball 下载线程的前置依赖`

### Implementation for US3

- [X] T012 [US3] Register test binary in `test-suit/starryos/normal/qemu-smp1/test-nix-prereqs/qemu-x86_64.toml`: add `test-unshare-fs` to `test_commands` (+1 entry, total 13)
- [X] T013 [US3] Run `cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs` in container — verify RED (fails on CLONE_FS before kernel fix applied, GREEN after)
- [X] T014 [US3] After kernel fix (T004-T005): re-run test-nix-prereqs → verify 13/13 PASSED

**Checkpoint**: 13/13 回归测试通过（含新增 test-unshare-fs）。

---

## Phase 4: Broad Regression — syscall suite (Priority: P2)

**Purpose**: 确认 CLONE_FS 修复不影响其他 syscall/unshare 路径。

- [X] T015 Run `cargo xtask starry test qemu --arch x86_64 -c syscall` in container:
  ```bash
  podman run ... cargo xtask starry test qemu --arch x86_64 -c syscall
  ```
  Expected: PASSED, no new regressions.

**Checkpoint**: syscall 全量回归通过。

---

## Phase 5: US4 — silicalet/ 过程笔记 (Priority: P2)

- [X] T016 [US4] Update `silicalet/002-nix-smoke.md`: record CLONE_FS fix decision, implementation status, validation results, and follow-up items (nixpkgs test still blocked by other unshare calls in Nix)

**Checkpoint**: 过程笔记覆盖修复始末。

---

## Dependencies & Execution Order

### Phase Dependencies

```
Phase 1 (Setup)
    │
    ▼
Phase 2 (US2: CLONE_FS fix) ──────────────────────┐
    │                                               │
    ▼                                               │
Phase 3 (US3: test-unshare-fs) ── T009-T011 FIRST  │
    │    (write test, verify RED)                   │
    │    then apply T004-T005 kernel fix             │
    │    then verify GREEN (T013-T014)              │
    │                                               │
    ▼                                               │
Phase 4 (syscall regression) ◄──────────────────────┘
    │
    ▼
Phase 5 (US4: silicalet notes)
```

### Within Each Phase

| Phase | Sequential Chain | Parallel Tasks |
|-------|------------------|---------------|
| Phase 1 | None | T001, T002, T003 all [P] |
| Phase 2 | T004→T005(sequential) → T006→T007→T008 | None (same file) |
| Phase 3 | T009, T010, T011 all [P] → T012 → T013 → T014 | T009, T010, T011 |
| Phase 4 | T015 alone | None |
| Phase 5 | T016 alone | None |

### TDD Order (Critical)

```
T009, T010, T011 (write test — verify RED)
    │  test calls unshare(CLONE_FS), expects success
    │  without kernel fix → test FAILS (EINVAL)
    ▼
T004, T005 (kernel fix — CLONE_FS in SUPPORTED_NS_FLAGS + unshare branch)
    │
    ▼
T013, T014 (re-run test — verify GREEN)
    │  test now PASSES — unshare(CLONE_FS) returns 0
```

---

## Implementation Strategy

### MVP First (Phase 2 + Phase 3 test-only)

1. T001-T003: Baseline确认
2. T009-T011: 先写回归测试（RED — CLONE_FS 未支持）
3. T004-T007: 内核修复（GREEN — CLONE_FS 返回 0）
4. T012-T014: 回归测试验证（13/13 PASSED）
5. **STOP** — MVP 完成

### Full Delivery

6. T015: syscall 全量回归
7. T016: silicalet 笔记
8. 全部 gate 通过 → 就绪提交

---

## Notes

- [P] tasks = different files, no dependencies on other in-progress tasks
- [US2] label = CLONE_FS 内核修复（解除 Nix 下载阻塞）
- [US3] label = test-unshare-fs 回归测试
- [US4] label = silicalet/ 过程笔记
- Kernel code change: `os/StarryOS/kernel/src/syscall/task/namespace.rs`
- Test change: `test-suit/starryos/normal/qemu-smp1/test-nix-prereqs/`
- All validation: container with `.ci-cache/` mounts per Constitution Rule III
- TDD order is CRITICAL: test must fail RED before kernel fix applied
