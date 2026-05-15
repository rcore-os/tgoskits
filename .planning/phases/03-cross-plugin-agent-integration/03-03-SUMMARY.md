---
phase: 03-cross-plugin-agent-integration
plan: 03
subsystem: agent-integration
tags: bug-hunt, test-driven-development, dispatching-parallel-agents, silent-failure-hunter, superpowers, pr-review-toolkit

# Dependency graph
requires:
  - phase: 01-foundation
    provides: Agent preamble standardization pattern (ABORT gate, Dependency Check)
provides:
  - bug-hunt.md with 7 frontmatter skills (2 new: test-driven-development, dispatching-parallel-agents)
  - bug-hunt preamble Agents section listing pr-review-toolkit:silent-failure-hunter with fallback and extended Design note
  - bug-hunt body Global Capabilities referencing TDD and dispatching-parallel-agents
  - bug-hunt body Phase 2 REPRO enhanced to RED Phase with RED verification and conditional silent-failure-hunter spawn
  - bug-hunt body Phase 3 FIX enhanced to GREEN Phase with minimal-fix guidance
  - bug-hunt body Phase 4 VERIFY enhanced to REFACTOR Phase with cleanup guidance
affects: [03-cross-plugin-agent-integration]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Frontmatter skills: superpowers:test-driven-development and superpowers:dispatching-parallel-agents added for RED/GREEN/REFACTOR methodology and parallel repro execution"
    - "Preamble Agents: pr-review-toolkit:silent-failure-hunter with fallback for manual error-handling analysis when sub-agent unavailable"
    - "Body sub-agent spawn: conditional on bug classification (error-handling issues only), with graceful degradation"

key-files:
  created: []
  modified:
    - .claude/agents/bug-hunt.md

key-decisions:
  - "test-driven-development skill added for RED/GREEN/REFACTOR methodology integrated into existing repro-fix-verify flow per D-04"
  - "dispatching-parallel-agents skill added for parallel execution of multiple independent repro scenarios per CPI-07"
  - "silent-failure-hunter spawn uses full pr-review-toolkit: prefix per RESEARCH.md Pitfall 1 to avoid collision with standalone plugins"
  - "TDD phases are additive to existing REPRO/FIX/VERIFY phases per D-07 — no structural changes to workflow"
  - "silent-failure-hunter conditional on error-handling classification (validation-bug, resource-bug: error code) per CPI-12"

patterns-established:
  - "Skill additions to existing agents follow additive-only approach: new content alongside existing, never replacing existing workflow steps"
  - "Sub-agent spawn includes both spawn instruction text AND fallback documentation for graceful degradation"
  - "RED/GREEN/REFACTOR phases map to existing Phase 2/3/4 without structural workflow changes"

requirements-completed: [CPI-06, CPI-07, CPI-12]

# Metrics
duration: 5min
completed: 2026-05-14
---

# Phase 03 Plan 03: Add TDD + parallel-agents skills and silent-failure-hunter spawn to bug-hunt

**bug-hunt agent updated with superpowers:test-driven-development and superpowers:dispatching-parallel-agents frontmatter skills, pr-review-toolkit:silent-failure-hunter in preamble Agents section, TDD RED/GREEN/REFACTOR phases integrated into REPRO/FIX/VERIFY workflow, and conditional silent-failure-hunter spawn for error-handling bug investigations**

## Performance

- **Duration:** 5 min
- **Started:** 2026-05-14T01:40:00Z
- **Completed:** 2026-05-14T01:45:XXZ
- **Tasks:** 2
- **Files modified:** 1

## Accomplishments
- Added `superpowers:test-driven-development` and `superpowers:dispatching-parallel-agents` to bug-hunt frontmatter skills list (7 total) and preamble Skills section
- Added `pr-review-toolkit:silent-failure-hunter` to preamble Agents section with full plugin prefix, fallback documentation, and extended Design note
- Added TDD and dispatching-parallel-agents references in body Global Capabilities for methodology guidance
- Enhanced Phase 2 REPRO to RED Phase with TDD RED verification step and conditional silent-failure-hunter spawn for error-handling bug classifications
- Enhanced Phase 3 FIX to GREEN Phase with minimal-fix guidance (smallest correct change)
- Enhanced Phase 4 VERIFY to REFACTOR Phase with cleanup and test-minimization guidance
- Preserved all existing workflow phases (1-5), bug classification tables, Synchronization Boundary Audit, lockdep sections, ABORT gate, and Rules

## Task Commits

Each task was committed atomically (single commit for both tasks since they modify the same file and the file was newly tracked in this worktree):

1. **Task 1: Add TDD and parallel-agents skills to frontmatter, silent-failure-hunter to preamble Agents** - `3860137` (feat)
2. **Task 2: Enhance REPRO/FIX/VERIFY phases with TDD structure and add conditional silent-failure-hunter spawn** - `3860137` (feat, same commit)

## Files Created/Modified
- `.claude/agents/bug-hunt.md` - Updated frontmatter skills (5 -> 7), preamble Skills section (2 -> 4 superpowers skills), preamble Agents section (None -> silent-failure-hunter with fallback), body Global Capabilities, Phase 2/3/4 enhancements with TDD RED/GREEN/REFACTOR, and conditional silent-failure-hunter spawn

## Decisions Made
- **Full plugin prefix**: Used `pr-review-toolkit:silent-failure-hunter` per RESEARCH.md Pitfall 1 to avoid collision with any standalone silent-failure-hunter plugin
- **Skills order**: Added new skills at end of frontmatter list (maintaining existing order), placed in alphabetical-by-plugin order in preamble Skills section for visibility
- **TDD phase mapping**: RED -> REPRO (test must fail on buggy code), GREEN -> FIX (minimal fix to pass), REFACTOR -> VERIFY (cleanup after confirmation)
- **silent-failure-hunter condition**: Spawned only when bug root cause is validation-bug or resource-bug(error-code), conserving overhead for logic/concurrency bugs
- **Single commit**: Both tasks committed together because the file was newly added to the worktree's git tracking (previously `.claude/agents/` was not tracked in this worktree)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

- `.claude/agents/` directory was not present in the worktree (only tracked `.claude/skills/` files exist in git). The file lives at `/home/rimuru/Projects/Code/homework/OS/tgoskits/.claude/agents/bug-hunt.md` in the main repo. For git tracking, the `.claude/agents/` directory was created in the worktree and the file was committed there. Both locations now have the same content.

## Stub Scan

No stubs found. The file is an agent definition document with no data rendering paths.

## Threat Flags

No new threat surface introduced beyond what the plan's threat model covers. All mitigations from threat model are implemented:
- T-03-06 (Spoofing: skill name): Mitigated via official marketplace skills `superpowers:test-driven-development` and `superpowers:dispatching-parallel-agents`
- T-03-07 (Spoofing: sub-agent spawn): Mitigated via full prefix `pr-review-toolkit:silent-failure-hunter`
- T-03-08 (DoS: sub-agent unavailability): Mitigated via fallback manual error-handling analysis
- T-03-09 (Elevation of Privilege: parallel dispatching): Mitigated via existing agent capabilities, no new permissions

## Self-Check: PASSED

- `.claude/agents/bug-hunt.md` exists at both worktree and main repo paths
- Commit `386013754` found in git log
- Frontmatter YAML parses correctly (7 skills)
- All verification grep counts >= 1
- No unintended deletions
- All existing workflow sections preserved
- Phase headers: Phase 2 with RED, Phase 3 with GREEN, Phase 4 with REFACTOR, Phase 5 unchanged

## Next Phase Readiness

- Ready for subsequent phase 03 plans (03-04: impl, 03-05: pr-review) which integrate skills and agents into remaining agent definitions
- Pattern established for adding TDD methodology and conditional sub-agent spawns to agent workflows

---
*Phase: 03-cross-plugin-agent-integration*
*Completed: 2026-05-14*
