---
phase: 1
slug: foundation-risk-mitigation
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-05-13
---

# Phase 1 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Python `unittest` + shell (`bash` + `grep`) integration tests |
| **Config file** | none — Wave 0 installs test scripts |
| **Quick run command** | `grep -rn "security-auditor" .claude/agents/ \| wc -l` |
| **Full suite command** | `bash .claude/scripts/test_preamble_consistency.sh && bash .claude/scripts/test_frontmatter_tools.sh && grep -rn "security-auditor" .claude/agents/ \| wc -l` |
| **Estimated runtime** | ~5 seconds |

---

## Sampling Rate

- **After every task commit:** Run `grep -rn "security-auditor" .claude/agents/ | wc -l`
- **After every plan wave:** Run full suite
- **Before `/gsd-verify-work`:** Full suite must be green
- **Max feedback latency:** 5 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 01-T3 | 01 | 1 | FND-01 | — | N/A | unit | `grep -c "superpowers" CLAUDE.md && grep -c "pr-review-toolkit" CLAUDE.md && grep -c "claude plugins install" CLAUDE.md` | ❌ W0 | ⬜ pending |
| 02-T3 | 02 | 1 | FND-04 | — | N/A | unit | `grep -A20 '^tools:' .claude/agents/pr-review.md \| grep WebSearch` | ❌ W0 | ⬜ pending |
| 02-T3 | 02 | 1 | FND-04 | — | N/A | unit | `grep -A20 '^tools:' .claude/agents/bug-hunt.md \| grep WebFetch` | ❌ W0 | ⬜ pending |
| 02-T3 | 02 | 1 | FND-05 | — | N/A | unit | `grep -A20 '^skills:' .claude/agents/pr-review.md \| grep superpowers:systematic-debugging` | ❌ W0 | ⬜ pending |
| 02-T2 | 02 | 1 | FND-03 | — | N/A | unit | `grep -rn "security-auditor" .claude/agents/ \| wc -l` (expect 0) | ❌ W0 | ⬜ pending |
| 01-T2 | 01 | 1 | FND-02 | — | N/A | unit | `python3 .claude/scripts/validate-deps.py; echo $?` (expect 0 with all plugins present) | ❌ W0 | ⬜ pending |
| 01-T2 | 01 | 1 | FND-02 | — | N/A | unit | `python3 .claude/scripts/validate-deps.py` (expect 1 with superpowers missing — mock) | ❌ W0 | ⬜ pending |
| 02-T1 | 02 | 1 | FND-02 | — | N/A | integration | `bash .claude/scripts/test_preamble_consistency.sh` (structural preamble consistency) | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `.claude/scripts/test_validate_deps.py` — unit tests for validation script with mock installed_plugins.json
- [ ] `.claude/scripts/test_preamble_consistency.sh` — structural consistency check across all 6 agent preamble blocks
- [ ] `.claude/scripts/test_frontmatter_tools.sh` — verify WebSearch/WebFetch present in pr-review and bug-hunt

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Preamble block renders as visible error in Claude Code agent output | FND-02 | Cannot automate Claude Code agent invocation output in CI | Invoke each agent with a missing dependency and verify "AGENT ABORTED:" message appears |
| validate-deps.py hook fires before agent context loads | FND-02 | Hook timing requires live Claude Code session | Check hook execution order in Claude Code logs after agent invocation |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 10s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
