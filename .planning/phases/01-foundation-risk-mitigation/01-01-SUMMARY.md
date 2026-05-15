---
phase: 01-foundation-risk-mitigation
plan: 01
subsystem: testing
tags: [python, pytest, hooks, plugins, validation, session-start, dependency-check]
requires: []
provides:
  - SessionStart plugin validation hook (validate-deps.py)
  - Unit test suite with mock installed_plugins.json fixtures
  - Plugin dependency documentation in CLAUDE.md
affects: [02-self-evolve-enhancement]
tech-stack:
  added: [pytest 9.0.3 (via venv)]
  patterns:
    - SessionStart hook registration in hooks.json
    - Python importlib.util dynamic module loading for hyphenated filenames
    - unittest.mock.patch stderr capture for hook error message testing
key-files:
  created:
    - .claude/scripts/validate-deps.py
    - .claude/scripts/test_validate_deps.py
  modified:
    - .claude/hooks/hooks.json
    - CLAUDE.md
    - .gitignore
key-decisions:
  - "pytest installed via venv at /tmp/pytest-venv to avoid externally-managed Python environment"
  - "Test loading of validate-deps uses importlib.util.spec_from_file_location for hyphenated filename"
  - ".pytest_cache/ added to .gitignore as Rule 2 auto-fix (generated test artifact)"
patterns-established:
  - "SessionStart hooks registered as first array entry in hooks.json, before PreToolUse hooks"
  - "Hook scripts expose check_plugins(plugins_path=None) function for test injection"
  - "Error messages use BLOCKED: prefix on stderr with batch install command"
requirements-completed:
  - FND-01
  - FND-02
duration: 10min
completed: 2026-05-13
---

# Phase 1 Plan 1: Plugin Dependency Validation Hook

**SessionStart validation hook with unit tests, mock fixtures, and plugin dependency documentation in CLAUDE.md**

## Performance

- **Duration:** 10 min
- **Started:** 2026-05-13T13:55:16Z
- **Completed:** 2026-05-13T14:05:00Z
- **Tasks:** 3
- **Files modified:** 5

## Accomplishments

- Created test_validate_deps.py with 4 unittest test cases using mock installed_plugins.json fixtures
- Implemented validate-deps.py as SessionStart hook checking superpowers >= 5.1.0 and pr-review-toolkit
- Registered SessionStart hook as first entry in hooks.json
- Added Required Plugins section to CLAUDE.md with dependency table and batch install commands

## Task Commits

Each task was committed atomically:

1. **Task 1: Write failing tests** - `ecb57089e` (test)
2. **Task 2: Create validate-deps.py** - `a189a850c` (feat)
3. **Task 3: Register hook and add docs** - `5c85be37b` (feat)

**Plan metadata:** Pending final docs commit

## Files Created/Modified

- `.claude/scripts/test_validate_deps.py` - 4 unittest tests with mock installed_plugins.json fixtures, importlib.util dynamic module loading, patch-based stderr capture
- `.claude/scripts/validate-deps.py` - SessionStart hook: checks superpowers (>= 5.1.0) and pr-review-toolkit (any version); prints BLOCKED errors to stderr with batch install command; safe version parsing for "unknown" strings
- `.claude/hooks/hooks.json` - Added SessionStart hook as first array entry pointing to validate-deps.py
- `CLAUDE.md` - Added Required Plugins section with dependency table and install commands
- `.gitignore` - Added `.pytest_cache/` (Rule 2 deviation)

## Decisions Made

- Used importlib.util.spec_from_file_location to load the hyphenated module name (validate-deps.py) for test injection, rather than sys.path manipulation
- All test functions call `_get_check_plugins()` inline (not in setUp) so that missing implementation produces 4 FAILED in pytest rather than 1 ERROR
- pr-review-toolkit treated as "any version" (min_version=None) per D-02, with "unknown" version strings accepted as satisfying all minimum checks

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 2 - Missing Critical] Added .pytest_cache/ to .gitignore**
- **Found during:** Task 1 (test_validate_deps.py)
- **Issue:** Running pytest generated `.pytest_cache/` directory that was not gitignored, leaving untracked artifacts
- **Fix:** Added `.pytest_cache/` to existing Python cache section in `.gitignore`
- **Files modified:** `.gitignore`
- **Verification:** `git check-ignore .pytest_cache` now matches
- **Committed in:** ecb57089e (Task 1 commit)

**2. [Rule 3 - Blocking] pytest not installed in system Python**
- **Found during:** Task 1 (before first test run)
- **Issue:** `python3 -m pytest` failed with "No module named pytest". System Python 3.14 uses externally-managed environment (Arch Linux), preventing pip install
- **Fix:** Created a venv at `/tmp/pytest-venv` and installed pytest there
- **Files modified:** None (no file changes - venv is ephemeral)
- **Verification:** `/tmp/pytest-venv/bin/python -m pytest --version` returns 9.0.3
- **Committed in:** N/A (pre-task setup, no file changes)

---

**Total deviations:** 2 auto-fixed (1 missing critical, 1 blocking)
**Impact on plan:** Both essential for test infrastructure. No scope creep.

## Issues Encountered

- Python 3.14.4 on Arch Linux uses externally-managed environment (PEP 668), preventing direct pip install of pytest. Workaround: installed pytest in a Python venv at `/tmp/pytest-venv`. This venv path was used for all test verification commands.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- SessionStart validation hook active - blocks session at startup if superpowers or pr-review-toolkit are missing
- Unit test suite ready for expansion (additional test cases or the 6-behavior suite in Task 2)
- CLAUDE.md documents required plugins with install commands
- Next plan (01-02) can add Layer 2 preamble blocks and agent frontmatter modifications

## Self-Check: PASSED

All 3 task commits verified in git log:
- `ecb57089e` test: add failing tests for validate-deps.py
- `a189a850c` feat: implement validate-deps.py plugin validation hook
- `5c85be37b` feat: register SessionStart hook and add plugin dependency docs

All 5 created/modified files verified on disk:
- `.claude/scripts/validate-deps.py` - 117 lines, valid Python
- `.claude/scripts/test_validate_deps.py` - 139 lines, valid Python
- `.claude/hooks/hooks.json` - valid JSON with SessionStart as first entry
- `CLAUDE.md` - Required Plugins section with dependency table
- `01-01-SUMMARY.md` - this file

---
*Phase: 01-foundation-risk-mitigation*
*Completed: 2026-05-13*
