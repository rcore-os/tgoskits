# TGOSKits Project-Local Plugin Design

**Date**: 2026-05-09
**Status**: approved
**Deliver**: first design all components, implement in batches

## 1. Plugin Structure

```
.claude/
├── plugin.json
├── settings.json
├── CLAUDE.md
├── skills/                         # existing 6 skills (unchanged)
│   ├── review-open-prs/
│   ├── cross-kernel-driver/
│   ├── starry-test-suit/
│   ├── arceos-test-adapter/
│   ├── board-uboot-fsck-repair/
│   └── update-std-tests/
├── commands/
│   ├── test.md
│   └── pr-prep.md
├── agents/
│   ├── pr-review.md
│   ├── test-gen.md
│   ├── bug-hunt.md
│   └── driver-audit.md
├── hooks/
│   ├── post-tool-use-log.md
│   ├── pre-pr-gate.md
│   └── session-end-journal.md
├── scripts/
│   ├── local-ci.sh
│   ├── syscall-diff.py
│   └── journal-generator.py
├── config/
│   └── docker-ci.toml
└── cache/
    ├── docker-image-base.hash
    ├── docker-image-lvz.hash
    └── last-ci-result.json
```

Existing 6 skills remain in place; the plugin only adds new capabilities.

## 2. Docker Local CI Infrastructure

### 2.1 Image Strategy (local-first, 2 images)

**Base image** (`tgoskits-ci`): built from `container/Dockerfile`
**Axvisor-LVZ image** (`tgoskits-ci-lvz`): built from `container/Dockerfile.axvisor-lvz`

Per-image flow:

```
local image exists & no rebuild trigger? → use local
no local image? → docker build
  build success? → docker pull remote for comparison
    remote not found / hash differs? → docker push remote
    hash matches? → done
  build failure? → docker pull remote
    pull success? → use remote (fallback)
    pull failure? → ERROR, abort
rebuild triggered (Dockerfile/rust-toolchain changed)? → docker build
  success? → push remote, use local
  failure? → keep old local if exists, or pull remote
```

### 2.2 Rebuild Triggers

```toml
[base_image]
name = "tgoskits-ci"
dockerfile = "container/Dockerfile"
remote = "ghcr.io/seek-hope/tgoskits-container:latest"
rebuild_triggers = ["container/Dockerfile", "rust-toolchain.toml"]

[axvisor_lvz_image]
name = "tgoskits-ci-lvz"
dockerfile = "container/Dockerfile.axvisor-lvz"
remote = "ghcr.io/seek-hope/tgoskits-container-axvisor-lvz:latest"
rebuild_triggers = ["container/Dockerfile.axvisor-lvz", "container/Dockerfile", "rust-toolchain.toml"]
```

Hash stored in `.claude/cache/docker-image-base.hash` / `.claude/cache/docker-image-lvz.hash`.
Push requires `GITHUB_TOKEN` or `CR_PAT` env var; silently skip push and warn if absent.

### 2.3 Full CI Matrix

```toml
[full]
commands = [
    "cargo fmt --all -- --check",
    "cargo xtask clippy",
    "cargo xtask sync-lint",
    "cargo xtask test",
    "cargo xtask arceos test qemu --arch x86_64",
    "cargo xtask arceos test qemu --arch riscv64",
    "cargo xtask arceos test qemu --arch aarch64",
    "cargo xtask arceos test qemu --arch loongarch64",
    "cargo xtask starry test qemu --arch riscv64",
    "cargo xtask starry test qemu --arch aarch64",
    "cargo xtask starry test qemu --arch x86_64",
    "cargo xtask starry test qemu --arch loongarch64",
    "cargo xtask axvisor test qemu --arch aarch64",
    "cargo xtask axvisor test qemu --arch riscv64",
    "cargo xtask axvisor test qemu --arch loongarch64",
]
```

### 2.4 `scripts/local-ci.sh` Interface

```bash
./scripts/local-ci.sh full              # full CI matrix
./scripts/local-ci.sh quick             # fmt + clippy + sync-lint
./scripts/local-ci.sh test starry aarch64  # single-arch test
./scripts/local-ci.sh rebuild            # force rebuild images
./scripts/local-ci.sh rebuild --push     # rebuild + validate + push
```

## 3. Hooks

### 3.1 PostToolUse — Activity Logging

**Trigger**: After AI calls Edit/Write/Bash that modifies files
**Output**: Append to `log.md`

```markdown
## 2026-05-09 14:32 — 3 files changed

**Files**: `os/StarryOS/kernel/src/syscall/time.rs`, `components/starry-signal/src/types.rs`

**Summary**: Implement posix timer_create/timer_settime/timer_delete syscalls.
Added three entry functions in time.rs, added PosixTimer struct and TIMER_ABSTIME
flag in types.rs. timer_delete handles cleanup of untriggered itimers.
```

Rules:
- Record file list + intent/logic summary only; no diff (git tracks code changes)
- Merge multiple file changes in same conversation round into one entry
- Truncate summary at 500 chars per entry

### 3.2 PreToolUse — PR Gate

**Trigger**: Before `gh pr create` or `git push`
**Behavior**:
1. Verify current branch is based on upstream/dev latest
2. If not → block, instruct to `git fetch upstream dev && git checkout -b <branch> upstream/dev`
3. Check `.claude/cache/last-ci-result.json` for passing CI
4. If no passing CI → block, instruct to run `./scripts/local-ci.sh quick`

```toml
[pre_pr_gate]
require_local_ci = true
require_clean_base = true
block_direct_push = true
```

### 3.3 Journal Generation

**Trigger**: `/journal <task-name>` command or agent post-completion
**Behavior**: Read `log.md`, extract task-related entries, generate `[task-name]-journal.md`

```markdown
# Journal: fix-timer-syscalls

**Time**: 2026-05-09 14:00 ~ 16:30
**Branch**: feat/fix-timer-syscalls
**Files touched**: 7

## Task Summary
Fixed POSIX timer syscall behavior mismatches with Linux...

## Change Log
(extracted from log.md)

## Test Results
(local CI result summary)

## Key Decisions
1. Implemented timers at kernel layer rather than libc...
2. ...

## Open Issues
- timer_getoverrun not yet implemented (low priority)
```

## 4. Commands

### 4.1 `/test`

| Invocation | Docker command |
|------------|---------------|
| `/test` | `/test quick` |
| `/test quick` | `cargo fmt --all -- --check && cargo xtask clippy && cargo xtask sync-lint` |
| `/test full` | Full CI matrix (see docker-ci.toml) |
| `/test fmt` | `cargo fmt --all -- --check` |
| `/test clippy` | `cargo xtask clippy` |
| `/test starry aarch64` | `cargo xtask starry test qemu --arch aarch64` |
| `/test axvisor` | All 3 architectures for axvisor |

Flow: check image → `docker run --rm -v $PWD:/workspace <image> <command>` → capture output → write `.claude/cache/last-ci-result.json`

### 4.2 `/pr-prep`

5-phase workflow:

| Phase | Action |
|-------|--------|
| 1. Branch Setup | `git fetch upstream dev && git checkout -b <branch> upstream/dev` |
| 2. Coding | AI writes code; PostToolUse hook logs |
| 3. CI Loop | `./scripts/local-ci.sh full`; auto-fix failures, max 5 iterations |
| 4. Review Loop | PR-Review Agent checks semantics; auto-fix, retry CI, max 3 iterations |
| 5. PR Creation | push → generate PR body → `gh pr create` → `/journal` |

Loop limits:
- CI fix: max 5 iterations (pause and ask user if exceeded)
- Review → CI: max 3 iterations (prevent infinite loop)
- AI must summarize status to user between iterations

PR body template:
```markdown
## Summary
<one-line summary>

### 1. <issue title>

**Type**: <behavior-bug / memory-bug / concurrency-bug / crash-bug / access-bug / resource-bug / missing-feature>

**Analysis**: <root cause — which function/line, why it's wrong>

**Solution**: <what files were changed, what was done>

### 2. <issue title>

...

## Expected Behavior
- <item 1>
- <item 2>
```

## 5. Agents

### 5.1 Bug-Hunt Agent

5-phase pipeline:

| Phase | Name | Actions |
|-------|------|---------|
| 1 | HUNT | Run reference Linux test in Docker (strace + stdout + stderr + exit code); run same test on target OS via QEMU; run `scripts/syscall-diff.py` to compare |
| 2 | REPRO | Analyze diff, classify bug (7 types), write minimal repro test, validate on Linux |
| 3 | FIX | Locate source, apply fix |
| 4 | VERIFY | Run repro test on OS, confirm output matches Linux; run `./scripts/local-ci.sh quick` |
| 5 | REPORT | Generate bug report, create git commit, optionally create PR |

Bug classification:
- **behavior-bug**: syscall return value/errno/output differs from Linux
- **crash-bug**: kernel panic, deadlock, infinite loop
- **memory-bug**: memory leak, use-after-free, double-free, buffer overflow
- **concurrency-bug**: race condition, unsynchronized shared state
- **access-bug**: unchecked user pointer, missing capability/permission check
- **resource-bug**: fd leak, integer overflow, resource exhaustion
- **missing-feature**: syscall/function entirely unimplemented

Tools: Read, Write, Edit, Bash, Grep, Glob
Skills: starry-test-suit, cross-kernel-driver, arceos-test-adapter

### 5.2 PR-Review Agent

Review dimensions:

| Dimension | Check | Severity |
|-----------|-------|----------|
| Syscall semantics | Return values, errno match POSIX/Linux | BLOCK |
| Boundary handling | NULL, 0, negative, overflow inputs | BLOCK |
| Resource leaks | fd not closed, unfreed alloc, unlocked mutex | BLOCK |
| Concurrency safety | Race conditions on shared state | WARN |
| Layer violation | Kernel calling ulib types directly | WARN |
| Test coverage | New syscall has corresponding test-suit case | INFO |

Flow: `git diff base...HEAD` → per-file review → auto-fix BLOCK items → re-run CI → re-review (max 3 iterations) → generate REVIEW.md

Skills: review-open-prs, starry-test-suit, arceos-test-adapter

### 5.3 Test-Gen Agent

Flow: study Linux reference behavior in Docker → generate C test with scenario coverage → configure toml → validate on Linux then on QEMU → output test files

Per-syscall coverage template:
- Normal path
- Invalid args (EINVAL, EFAULT, EAGAIN, etc.)
- Boundary values
- Resource exhaustion
- Concurrency (if applicable)

Skills: starry-test-suit, arceos-test-adapter

### 5.4 Driver-Audit Agent

4-layer audit:

| Layer | Check | Severity |
|-------|-------|----------|
| Driver Core | No OS-specific types, uses mmio-api | BLOCK |
| Capability Boundary | IRQ via contract, DMA via dma-api | BLOCK |
| OS Glue | Correct axplat dependencies, feature gates | WARN |
| Runtime | Registered with axdriver, devfs node created | INFO |

Output: `AUDIT.md` with file + line number violations

Skills: cross-kernel-driver

## 6. Syscall Diff Infrastructure

`scripts/syscall-diff.py` compares reference (Linux strace) vs actual (OS QEMU):

Input:
- `linux-trace.log`: `strace -f -v` output from Docker Linux
- `os-output.log`: stdout/stderr + exit code from OS QEMU run
- (optional) `os-trace.log`: if OS has strace-like capability

Comparison dimensions:
1. Syscall sequence: order and count of syscalls
2. Syscall args: parameter values per call
3. Syscall return values: success/error codes
4. Final output: stdout, stderr, exit code

## 7. Implementation Batches (TBD)

To be detailed in implementation plan.
