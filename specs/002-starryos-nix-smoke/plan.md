# Implementation Plan: CLONE_FS unshare 支持 (Phase 1/N)

**Branch**: `002-starryos-nix-smoke` | **Date**: 2026-06-08 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/002-starryos-nix-smoke/spec.md`

## Incremental Delivery Strategy

本 spec 描述的完整 Nix smoke 验证流程（选工作流 → 跑 Nix → 捕获失败 → 分类 → 逐个修复 → 回归测试 → 笔记 → 交付）通过多个增量 Phase 交付。每个 Phase 解决一个独立的 Nix 阻塞点。

| Phase | 内容 | 覆盖的 FR | 状态 |
|-------|------|-----------|------|
| **Phase 1** (本文档) | `CLONE_FS` unshare 支持 + 回归测试 | FR-006, FR-008, FR-011~FR-014 | 🔧 实施中 |
| Phase 2 | 其他 unshare 阻塞点 (CLONE_NEWNS 等) — 调查后确定 | FR-003~FR-005, FR-006 | 📋 待规划 |
| Phase 3 | nixpkgs 端到端 smoke (`nix build nixpkgs#hello`) | FR-001, FR-002, FR-009, SC-001, SC-003 | 📋 待规划 |
| Phase 4 | 收尾: 状态总结、候选 PR 内容、交付 | FR-010, US5, SC-005, SC-007 | 📋 待规划 |

**当前 Phase 1 范围**：
- ✅ FR-006: 为 CLONE_FS 行为缺口产生回归测试
- ✅ FR-008: 修复验证与 app-test 分离
- ✅ FR-011~FR-014: silicalet 笔记、上游同步
- ❌ FR-001~FR-005, FR-007, FR-009, FR-015: 属于后续 Phase
- ❌ US1, US5: 属于后续 Phase
- ❌ SC-001, SC-003: 属于后续 Phase

## Summary

在 `sys_unshare` 中添加 `CLONE_FS` 支持，使 Nix 下载子系统可正常工作。
同时在 `test-nix-prereqs` 中补充回归测试，标记为 "nixpkgs 可能用到"。
此修复是 nixpkgs smoke 测试的前置条件。

## Technical Context

**Language/Version**: Rust nightly-2026-05-28, edition 2024

**Primary Dependencies**: starry-kernel, ax-fs-ng, axnsproxy

**Storage**: N/A

**Testing**: C regression tests (CMake + test_framework.h) under `test-suit/starryos/normal/qemu-smp1/test-nix-prereqs/`

**Target Platform**: StarryOS x86_64 QEMU

**Project Type**: OS kernel + test-suit

**Performance Goals**: N/A — correctness fix, no perf change

**Constraints**: Must pass `cargo fmt --check`, `cargo xtask clippy --since origin/dev`, no `[patch.crates-io]`

**Scale/Scope**: ~2 kernel files changed, 1 new C regression test added to existing test-nix-prereqs grouped case

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Rule | Status | Evidence |
|------|--------|----------|
| I. Code Style (`cargo fmt`) | ✅ | Will run `cargo fmt` on changed files |
| II. Clippy (no warnings, no allow) | ✅ | Will run `cargo xtask clippy --package starry-kernel` |
| III. Build/Test via `cargo xtask` | ✅ | `cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs` |
| IV. Driver Layer Isolation | N/A | No driver changes |
| V. PR Standards (title/body) | 🔧 | Plan only — PR body for implementation |
| VI. Code Ownership | N/A | No new code owners needed |
| No `[patch.crates-io]` | ✅ | Confirmed absent |

**Container validation path**:
```bash
podman run --rm --userns=keep-id \
  -v $PWD:/workspace -w /workspace \
  -v $PWD/.ci-cache/cargo:/tmp/cargo \
  -v $PWD/.ci-cache/rustup:/tmp/rustup \
  -e CARGO_HOME=/tmp/cargo -e RUSTUP_HOME=/tmp/rustup \
  ghcr.io/rcore-os/tgoskits-container:latest \
  cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs
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
└── tasks.md             # Phase 2 output (/speckit-tasks)
```

### Source Code (repository root)

```text
# Kernel fix
os/StarryOS/kernel/src/syscall/task/
└── namespace.rs         # +CLONE_FS to SUPPORTED_NS_FLAGS, +unshare_fs branch

# Regression test
test-suit/starryos/normal/qemu-smp1/test-nix-prereqs/
├── qemu-x86_64.toml     # +1 test_commands entry
└── test-unshare-fs/
    └── c/
        ├── CMakeLists.txt
        └── src/main.c   # NEW: unshare(CLONE_FS) + fs isolation verification

# App smoke (no changes needed — nixpkgs test already depends on this fix)
apps/starry/nix/
└── nix-nixpkgs.sh       # existing, expects CLONE_FS to work
```

**Structure Decision**: Kernel fix stays in `syscall/task/namespace.rs` where `sys_unshare` lives. Regression test follows the grouped `test-nix-prereqs` pattern as a C subcase under `test-unshare-fs/`. The `test-nix-prereqs/qemu-x86_64.toml` already lists 12 test_commands; this adds a 13th.

## Complexity Tracking

> No constitution violations to justify.

---

## Phase 0: Research — CLONE_FS Semantics

### Decision 1: CLONE_FS behaviour for StarryOS

**Decision**: `unshare(CLONE_FS)` creates an independent `FsContext` copy for the calling task.

**Rationale**: On Linux, `unshare(CLONE_FS)` disassociates filesystem attributes (root, cwd, umask) shared via `clone(CLONE_FS)`. In StarryOS:
- By default, each task already has an independent `FsContext` (clone without CLONE_FS auto-forks)
- `clone(CLONE_FS)` causes child to `clone_from(&FS_CONTEXT)` (share parent's FsContext)
- So `unshare(CLONE_FS)` reverses this: clone the current FsContext to break sharing

**Alternatives considered**:
- No-op: rejected — tasks created with clone(CLONE_FS) would still share FsContext after unshare
- Only add flag validation: rejected — must implement actual unshare semantics

### Decision 2: Implementation approach

**Decision**: Add CLONE_FS to `SUPPORTED_NS_FLAGS` and add a Phase 2 branch that clones FsContext.

**Rationale**: The existing code already separates Phase 1 (spinlock nsproxy ops) and Phase 2 (FsContext ops). CLONE_FS fits in Phase 2 alongside CLONE_NEWNS.

**Alternative considered**: Adding a separate Phase 3 — unnecessary; both are FsContext-level mutations.

### Decision 3: Regression test design

**Decision**: A two-process test: parent `clone(CLONE_FS)` to share FsContext, child calls `unshare(CLONE_FS)`, then both independently change cwd and verify the other is unaffected.

**Rationale**: This proves that `unshare(CLONE_FS)` actually breaks FsContext sharing. A simpler test (just unshare + check return code) wouldn't verify semantic correctness.

---

## Phase 1: Design & Contracts

### Data Model

No new entities. The fix touches two existing structures:

| Entity | Change |
|--------|--------|
| `SUPPORTED_NS_FLAGS` (namespace.rs:13) | Add `CLONE_FS` bit |
| `sys_unshare()` (namespace.rs:17) | Add CLONE_FS branch → clone FsContext |
| `FsContext` (axfs-ng/highlevel/fs.rs) | Already has `Clone` impl — no changes |

### Contracts

#### Contract: unshare(CLONE_FS).md

```text
# unshare(CLONE_FS) Interface Contract

## sys_unshare(flags = CLONE_FS)

Returns: Ok(0) on success, Err(EINVAL) if flags contain unsupported bits.

Pre-condition: Task has a valid FsContext.

Post-condition: Calling task's FsContext is independent — root_dir and current_dir
mutations do not affect any other task.

## clone(CLONE_FS)

When CLONE_FS is set: child shares parent's FsContext (clone_from).
unshare(CLONE_FS) must reverse this.

## Regression: test-unshare-fs

1. Parent clone(CLONE_FS) → child shares parent FsContext
2. Child unshare(CLONE_FS) → child gets independent FsContext
3. Parent chdir("/tmp"), child chdir("/nix") → verify each has own cwd
4. Verify fchdir back to original and stat both paths
```

### CI-Like Validation

Per Constitution Rule III, all validation MUST use the CI container path.
Host-only shortcuts are recorded as fallback only.

#### Mandatory validation gates

```bash
# ── Gate 1: Formatting ──
cargo fmt --check os/StarryOS/kernel/src/syscall/task/namespace.rs

# ── Gate 2: Clippy (container) ──
podman run --rm --userns=keep-id \
  -v $PWD:/workspace -w /workspace \
  -v $PWD/.ci-cache/cargo:/tmp/cargo \
  -v $PWD/.ci-cache/rustup:/tmp/rustup \
  -e CARGO_HOME=/tmp/cargo -e RUSTUP_HOME=/tmp/rustup \
  ghcr.io/rcore-os/tgoskits-container:latest \
  cargo xtask clippy --package starry-kernel

# ── Gate 3: test-nix-prereqs (container, 12→13 tests) ──
podman run --rm --userns=keep-id \
  -v $PWD:/workspace -w /workspace \
  -v $PWD/.ci-cache/cargo:/tmp/cargo \
  -v $PWD/.ci-cache/rustup:/tmp/rustup \
  -e CARGO_HOME=/tmp/cargo -e RUSTUP_HOME=/tmp/rustup \
  ghcr.io/rcore-os/tgoskits-container:latest \
  cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs

# ── Gate 4: syscall regression (container, covers wider unshare impact) ──
podman run --rm --userns=keep-id \
  -v $PWD:/workspace -w /workspace \
  -v $PWD/.ci-cache/cargo:/tmp/cargo \
  -v $PWD/.ci-cache/rustup:/tmp/rustup \
  -e CARGO_HOME=/tmp/cargo -e RUSTUP_HOME=/tmp/rustup \
  ghcr.io/rcore-os/tgoskits-container:latest \
  cargo xtask starry test qemu --arch x86_64 -c syscall
```

#### Expected results

| Gate | Target | Expected |
|------|--------|----------|
| fmt | `namespace.rs` | clean |
| clippy | `starry-kernel` | 0 warnings |
| test-nix-prereqs | 13 grouped C tests | 13/13 PASSED |
| syscall | full syscall suite | PASSED (no regressions) |

#### Host fallback (if container unavailable)

```bash
# Record reason in silicalet/; prefer container if possible.
cargo fmt --check
cargo xtask clippy --package starry-kernel
cargo xtask starry test qemu --arch x86_64 -c test-nix-prereqs
```
