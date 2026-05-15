---
name: impl
description: Implement missing StarryOS features — syscalls, libraries, or full binary support
args:
  - name: prompt
    type: string
    required: true
---

# /impl — Feature Implementation Workflow

Before starting, invoke `superpowers:verification-before-completion` to confirm a clean working tree and that you're on a reasonable base branch.

Read `.claude/agents/impl.md` and follow its 6-phase workflow:

1. **DISCOVER** — Analyze the target (binary analysis if software, strace capture, syscall-diff gap analysis, research)
2. **PLAN** — Classify by priority (P0/P1/P2), assign kernel/ulib layers, find reference implementations, auto-decide stub vs full
3. **TEST** — Delegate to `test-gen` agent; validate on Linux before proceeding
4. **IMPLEMENT** — Write code for P0 → P1 → P2 stubs; verify with syscall-diff; iterate until all P0 match Linux
5. **CI LOOP** — `local-ci.sh full`, fix, retry (max 5 iterations)
6. **PR** — Delegate to `pr-review` agent for self-review; generate feature-oriented PR body; create PR from clean cherry-picked branch

## Input

The full prompt after `/impl` is the user's goal. Auto-detect the mode:

| Mode | Trigger | Start Phase |
|------|---------|-------------|
| **Binary** | "run", "support running", "execute" | Phase 1a (binary analysis) |
| **Syscall** | Syscall names, "implement", "add support for" | Phase 1b (strace capture with reproducer) |

## Key Rules

- **Never skip the gap analysis** (Phase 1c). It's the foundation for everything else.
- **Always write tests before implementation** (Phase 3 before Phase 4). Tests are the spec.
- **Treat syscall-diff as the ground truth** for behavior correctness (Phase 4e).
- **P2 items get stubs, not full implementations** — don't over-engineer.
- **Stub decisions are auto-made** using the heuristics in the agent file; report the rationale.
