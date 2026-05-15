---
phase: 02-self-evolve-enhancement
plan: 03
subsystem: agent-workflow
tags: self-evolve, plugin-dev, brainstorming, systematic-debugging

# Dependency graph
requires:
  - phase: 02-01
    provides: Frontmatter skills (plugin-dev, superpowers) and Dependency Check preamble
  - phase: 02-02
    provides: D2/D3 plugin-validator sub-agent automation, D4 cross-ref extension with collision detection
provides:
  - D1 section invokes plugin-dev:plugin-structure for file/directory layout verification
  - D6 section invokes plugin-dev:plugin-settings for settings/config consistency checks
  - D7 section invokes plugin-dev:hook-development for hook structure validation
  - New Brainstorming Phase before audit rounds using superpowers:brainstorming explore/clarify/propose/prioritize methodology
  - Step 3: Fix enhanced with superpowers:systematic-debugging root-cause analysis (Iron Law)
  - Step 5 report format updated with root-cause tracking
affects: future self-evolve audit cycles, plugin-dev integration patterns

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Inline skill invocation references follow selective-by-dimension loading (D-03)"
    - "Brainstorming phase runs once before first audit round (Pitfall 4 avoidance)"
    - "Systematic-debugging cross-cut applied when quality issues found during Fix step"

key-files:
  created: []
  modified:
    - .claude/agents/self-evolve.md

key-decisions:
  - "Skill invocations are inline prose references (invoke `plugin-dev:plugin-structure`) not restructured sections"
  - "Existing D1-D7 round structure preserved - skills enhance, do not replace, existing checks"
  - "Brainstorming runs once before round 1 (not between rounds) per RESEARCH.md OQ #2 recommendation"
  - "Systematic-debugging applied in Step 3 via Iron Law gate (no fix without root cause investigation)"

patterns-established:
  - "plugin-dev skills are loaded as inline methodology references, not as agent sub-spawns (except plugin-validator)"
  - "Brainstorming provides strategic lens before tactical audit rounds"
  - "Root-cause tracking in round reports prevents symptom-level patches from reappearing"

requirements-completed:
  - SE-07
  - SE-08

# Metrics
duration: ~10min
completed: 2026-05-14
---

# Phase 02 Plan 03: Self-Evolve Enhancement -- Skill Invocations and Workflow Additions

**D1/D6/D7 skill invocation references (plugin-structure, plugin-settings, hook-development), new Brainstorming Phase before audit rounds, and systematic-debugging cross-cut in Step 3 (Fix)**

## Performance

- **Duration:** ~10 min
- **Started:** 2026-05-14T09:00:00Z
- **Completed:** 2026-05-14T09:10:00Z
- **Tasks:** 2
- **Files modified:** 1

## Accomplishments

- Added `plugin-dev:plugin-structure` invocation to D1 before path checklist for file/directory layout verification
- Added `plugin-dev:plugin-settings` invocation to D6 before completeness checklist for settings/config consistency
- Added `plugin-dev:hook-development` invocation to D7 before hook checklist for structure and security validation
- Added Brainstorming Phase (before first round) with `superpowers:brainstorming` explore/clarify/propose/prioritize methodology
- Enhanced Step 3: Fix with `superpowers:systematic-debugging` root-cause analysis (Iron Law: no fixes without root cause investigation)
- Updated Step 5 round report format to include root-cause tracking
- All existing D1-D7 round structure, D5 (Anti-patterns), and Final Report section preserved unchanged

## Task Commits

Each task was committed atomically:

1. **Task 1: Add plugin-dev skill invocation references to D1, D6, D7** - `a04d1910d` (feat)
2. **Task 2: Add brainstorming phase and systematic-debugging cross-cut to workflow** - `db1165c24` (feat)

**Plan metadata:** Pending via orchestrator.

## Files Created/Modified

- `.claude/agents/self-evolve.md` - Skill invocation references in D1/D6/D7 sections, new Brainstorming Phase section, enhanced Step 3 (Fix) with systematic-debugging, updated Step 5 report format

## Decisions Made

- Skill invocations are prose references ("invoke `plugin-dev:plugin-structure`") -- this is how Claude Code resolves skill references at runtime
- Existing checklist items preserved as the concrete verification layer; skill invocations provide structured methodology above them
- Brainstorming runs once before round 1 (not between rounds) per RESEARCH.md OQ #2 recommendation
- Systematic-debugging applied in Step 3 as a gate before fixes, not as a separate round step
- D5 (Anti-patterns) left completely unchanged -- no skill invocation needed there
- D6 uses `plugin-dev:plugin-settings` (not `mcp-integration`) per RESEARCH.md A1 confirmation

## Deviations from Plan

None -- plan executed exactly as written.

## Issues Encountered

**Worktree mismatch:** The worktree branch (`worktree-agent-a94e2df9ef293ab7d`) was at `b61f18d05` (upstream origin/main) while the base commit `1aa2248fc` is on the local `dev` branch. The `.claude/agents/` files exist only in `dev`'s working tree, not in the worktree's HEAD. Commits were made directly to the `dev` branch via `git -C /home/rimuru/Projects/Code/homework/OS/tgoskits`, consistent with the pattern used by previous waves (02-01, 02-02). This is a setup artifact of the worktree not being rebased onto the base commit at spawn time.

Resolved by: Using `git -C /home/rimuru/Projects/Code/homework/OS/tgoskits` to add and commit from the main repo's git context, targeting the `dev` branch where `.claude/agents/self-evolve.md` is tracked.

## Next Phase Readiness

All Phase 02 plans (02-01, 02-02, 02-03) are complete. The self-evolve agent now has:
- Full frontmatter skills loading (7 plugin-dev + superpowers skills) [02-01]
- Dependency Check preamble with fallback mode [02-01]
- D2/D3 plugin-validator sub-agent automation [02-02]
- D4 cross-ref extension with installed_plugins.json validation and agent-name collision detection [02-02]
- D1/D6/D7 skill invocation references [02-03]
- Brainstorming Phase for improvement ideation [02-03]
- Systematic-debugging cross-cut for root-cause analysis [02-03]

The self-evolve agent is feature-complete per the Phase 02 requirements.

## Self-Check: PASSED

- [x] SUMMARY.md exists at `.planning/phases/02-self-evolve-enhancement/02-03-SUMMARY.md`
- [x] Commit `a04d1910d` (Task 1: add plugin-dev skill invocation references to D1, D6, D7)
- [x] Commit `db1165c24` (Task 2: add brainstorming phase and systematic-debugging cross-cut to workflow)
- [x] D1 invokes `plugin-dev:plugin-structure` before path checklist
- [x] D6 invokes `plugin-dev:plugin-settings` before completeness checklist
- [x] D7 invokes `plugin-dev:hook-development` before hook checklist
- [x] D5 (Anti-patterns) section unchanged
- [x] Brainstorming Phase added before Round N with explore/clarify/propose/prioritize
- [x] Step 3: Fix enhanced with systematic-debugging and Iron Law reference
- [x] Step 5 report format includes root-cause tracking
- [x] Existing round structure (5 steps) and Final Report section preserved
- [x] No references to `plugin-dev:mcp-integration` (correctly uses `plugin-settings`)

---
*Phase: 02-self-evolve-enhancement*
*Completed: 2026-05-14*
