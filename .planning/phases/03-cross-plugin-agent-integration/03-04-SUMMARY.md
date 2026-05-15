---
phase: 03-cross-plugin-agent-integration
plan: 04
subsystem: agents
tags: [systematic-debugging, type-design-analyzer, code-simplifier, pr-review-toolkit, superpowers]

requires:
  - phase: 03-cross-plugin-agent-integration
    plan: 02
    provides: pr-review-toolkit agent definitions
  - phase: 03-cross-plugin-agent-integration
    plan: 03
    provides: superpowers plugin integration patterns
provides:
  - systematic-debugging skill integration into impl agent
  - type-design-analyzer conditional spawn in Phase 4a.5
  - code-simplifier conditional spawn in Phase 4c.5
affects: [impl agent usage, future agent updates]

tech-stack:
  added: [superpowers:systematic-debugging, pr-review-toolkit:type-design-analyzer, pr-review-toolkit:code-simplifier]
  patterns: [conditional sub-agent spawn with fallback, bracketed implementation lifecycle review]

key-files:
  created: []
  modified:
    - .claude/agents/impl.md

key-decisions:
  - "superpowers:systematic-debugging listed before verification-before-completion in preamble Skills section (new entries first)"
  - "Full pr-review-toolkit: prefix used for both sub-agents to avoid collision with standalone plugins (Research Pitfall 1)"
  - "type-design-analyzer spawns BEFORE method implementations (4a.5) to reduce cost of fixing type design after methods written"
  - "code-simplifier spawns BEFORE QEMU build (4c.5) so any behavioral changes are caught by syscall-diff in 4e"

patterns-established:
  - "Sub-agent spawn sections use pattern: spawn instruction with exact quote, guidance on incorporating results, fallback manual review path"
  - "Additive modifications only — no structural changes to existing 6-phase workflow per D-07"

requirements-completed:
  - CPI-08
  - CPI-13
  - CPI-14

duration: 1min
completed: 2026-05-14
---

# Phase 03 Plan 04: Impl Agent Systematic Debugging + Sub-Agent Spawns Summary

**Expanded impl agent with superpowers:systematic-debugging skill in frontmatter and two new conditional sub-agent spawns -- type-design-analyzer (4a.5) and code-simplifier (4c.5) -- bracketing the implementation lifecycle**

## Performance

- **Duration:** 1 min
- **Started:** 2026-05-14T01:42:31Z
- **Completed:** 2026-05-14T01:44:26Z
- **Tasks:** 2 (both auto)
- **Files modified:** 1

## Accomplishments

- Added `superpowers:systematic-debugging` to impl frontmatter (CPI-08)
- Added `pr-review-toolkit:type-design-analyzer` and `pr-review-toolkit:code-simplifier` to preamble Agents section (CPI-13, CPI-14, D-06)
- Added systematic-debugging reference to Global Capabilities for implementation debugging
- Added new `### 4a.5: Type/Trait Design Review` section that conditionally spawns type-design-analyzer before method implementations, with fallback manual review instructions
- Added new `### 4c.5: Post-Implementation Code Polish` section that spawns code-simplifier after implementation but before QEMU build, with fallback manual simplification pass
- Both sub-agent spawns use full `pr-review-toolkit:` prefix per Research Pitfall 1 avoidance
- Both sub-agent spawns have fallback: warn and continue with manual review if unavailable
- All existing 6 phases and section structure preserved per D-07

## Task Commits

Each task was committed atomically:

1. **Task 1-2: Add systematic-debugging skill and sub-agent spawns** - `26e860e9f` (feat)

**Plan metadata:** pending final docs commit

_Note: Both tasks modified the same file (impl.md) and were committed together._

## Files Created/Modified

- `.claude/agents/impl.md` - Added systematic-debugging skill, type-design-analyzer and code-simplifier sub-agent spawns, new Phase 4 sections (4a.5, 4c.5), Global Capabilities reference

## Decisions Made

- **systematic-debugging listed first in preamble Skills** -- new entries first per established pattern
- **Full plugin prefix for sub-agents** -- `pr-review-toolkit:type-design-analyzer` and `pr-review-toolkit:code-simplifier` avoid collision with standalone plugins (Research Pitfall 1)
- **type-design-analyzer in 4a.5 (before methods)** -- cheaper to fix type design before method implementations
- **code-simplifier in 4c.5 (before QEMU build)** -- behavioral changes caught by syscall-diff in Phase 4e
- **Additive only** -- no structural changes to existing 6-phase workflow per D-07

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None -- all edits applied cleanly, all verifications passed on first attempt.

## User Setup Required

None - no external service configuration required. Both `superpowers` and `pr-review-toolkit` are already installed plugins (Phase 03-02, 03-03).

## Next Phase Readiness

- Impl agent now has 4 skills in frontmatter (existing 3 + systematic-debugging)
- Preamble Agents lists 3 entries with fallback documentation
- Phase 4 has two new sub-agent spawn sections bracketing the implementation lifecycle
- Ready for Phase 03-05 (remaining integration tasks)

---
*Phase: 03-cross-plugin-agent-integration*
*Completed: 2026-05-14*
