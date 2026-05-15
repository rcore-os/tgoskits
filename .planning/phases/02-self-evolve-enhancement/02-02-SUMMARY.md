---
phase: 02-self-evolve-enhancement
plan: 02
subsystem: agent
tags: [self-evolve, plugin-validator, collision-detection, installed-plugins]

requires:
  - phase: 02-self-evolve-enhancement
    plan: 01
    provides: self-evolve frontmatter with 8 skills and expanded Dependency Check preamble
provides:
  - Combined D2+D3 section with plugin-validator sub-agent spawn (7 validation targets, severity mapping)
  - Extended D4 section with installed_plugins.json cross-reference for all agent files
  - Agent-name collision detection with exact basename matching
affects: 02-03 (subsequent plan references D2/D3/D4 structure defined here)

tech-stack:
  added: [plugin-dev:plugin-validator agent as spawnable sub-agent]
  patterns: [Sub-agent spawn for validation, installed_plugins.json cross-reference, exact basename collision detection]

key-files:
  modified:
    - .claude/agents/self-evolve.md

key-decisions:
  - "plugin-validator spawn is documented as body prose (not code block) following the same convention as impl.md's spawn references (D-01)"
  - "D4 cross-reference defines a two-pass approach: pass 1 uses installed_plugins.json (independent of plugin-dev), pass 2 for collision detection needs plugin-dev cache paths"
  - "Collision detection is warning-only INFO/WARN severity, never BLOCK, per D-04 non-blocking requirement"

patterns-established:
  - "Sub-agent spawn for validation: spawn once per cycle (D-06), pass all files in a single context window"
  - "installed_plugins.json parsing: use os.path.expanduser() for path resolution (Pitfall 3 avoidance)"
  - "Collision detection: exact basename comparison, not substring grep (Pitfall 5 avoidance)"

requirements-completed: [SE-01, SE-02, SE-06]

duration: 5min
completed: 2026-05-14
---

# Phase 02 Plan 02: D2/D3 plugin-validator Automation and D4 Cross-Reference Extension

**Replaced manual D2/D3 bash checks with plugin-validator sub-agent spawn, extended D4 with installed_plugins.json cross-reference for all agent files and agent-name collision detection**

## Performance

- **Duration:** 5 min
- **Started:** 2026-05-14T01:04:34Z
- **Completed:** 2026-05-14T01:05:42Z
- **Tasks:** 2
- **Files modified:** 1

## Accomplishments

- Replaced standalone D2 (Syntax) and D3 (Frontmatter) sections with a single combined D2+D3 section that spawns `plugin-dev:plugin-validator` once per audit cycle
- Documented 7 validation targets for the plugin-validator sub-agent: plugin.json manifest, agent files, command files, hooks, skill directories, file organization, naming conventions
- Added severity mapping: critical=BLOCK, major=WARN, minor=INFO
- Extended D4 with Subsection A: Cross-Plugin Skill Reference Validation that parses installed_plugins.json and validates all plugin:skill references across ALL `.claude/agents/*.md` files (D-02, SE-02)
- Extended D4 with Subsection B: Agent-Name Collision Detection that scans globally installed plugin agent directories and warns on exact-name matches with the 6 TGOSKits agent names (D-04, SE-06)
- Documented fallback paths for both D2+D3 and D4 when plugin-dev is not installed (D-05)
- Preserved existing D4 baseline checklist (4 items) and D5 section intact

## Task Commits

Each task was committed atomically:

1. **Task 1: Replace D2 and D3 sections with combined D2+D3 plugin-validator sub-agent spawn** - `40c394915` (feat)
2. **Task 2: Extend D4 cross-reference with installed_plugins.json validation and collision detection** - `d37955438` (feat)

Also included (cherry-picked from `dev` to bring worktree to base state):
- `5796f1ed9` - 02-01 Task 1 (frontmatter skills)
- `f52c96bbd` - 02-01 Task 2 (preamble rewrite)

## Files Modified

- `.claude/agents/self-evolve.md` - Replaced D2/D3 sections, extended D4 with two new subsections

## Decisions Made

- plugin-validator spawn documented in body prose (not a code block) following the same convention as impl.md's spawn references (D-01)
- D4 cross-reference two-pass approach: Subsection A works independently of plugin-dev (parses local installed_plugins.json); Subsection B requires plugin-dev for plugin cache access
- Collision detection severity capped at WARN/INFO, never BLOCK, per D-04 non-blocking requirement

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

- Worktree branch did not contain 02-01 commits at execution start. Cherry-picked the two 02-01 commits onto the worktree branch before applying 02-02 changes. This is normal worktree setup -- the orchestrator had applied 02-01 to `dev` branch, and the worktree needed those commits too.

## Next Phase Readiness

- D2/D3 now delegate to plugin-validator sub-agent (once per cycle)
- D4 covers cross-plugin skill validation across all agent files and future collision detection
- Ready for plan 02-03 (D1/D6/D7 skills integration + workflow/brainstorming phase)

---

*Phase: 02-self-evolve-enhancement*
*Completed: 2026-05-14*

## Self-Check: PASSED

- **Files verified:** `.claude/agents/self-evolve.md` (FOUND), `02-02-SUMMARY.md` (FOUND)
- **Commits verified:** `40c394915` (Task 1), `d37955438` (Task 2) -- both present in git log
- **Verification checks:** All grep checks passed (D2+D3 section, plugin-validator spawn, D4 extensions, collision detection, no orphaned D2/D3 sections)
- **No missing files or commits detected**
