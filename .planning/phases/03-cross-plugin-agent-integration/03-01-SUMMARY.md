---
phase: 03-cross-plugin-agent-integration
plan: 01
name: add-brainstorming-tdd-to-test-gen
subsystem: agent-integration
tags:
  - agent
  - test-gen
  - brainstorming
  - test-driven-development
  - frontmatter
  - body-references
requires:
  - CPI-09
  - CPI-10
provides:
  - test-gen loaded with brainstorming skill
  - test-gen loaded with test-driven-development skill
affects:
  - .claude/agents/test-gen.md
tech-stack:
  added:
    - superpowers:brainstorming
    - superpowers:test-driven-development
  patterns:
    - frontmatter-only skill loading (D-05)
    - additive only, no structural changes (D-07)
key-files:
  created: []
  modified:
    - .claude/agents/test-gen.md
decisions:
  - "Skills loaded via frontmatter only per D-05 from Phase 03 context"
  - "No structural changes to test-gen workflow per D-07 (additive only)"
  - "No preamble changes — test-gen has zero spawn targets per D-05"
metrics:
  duration: null
  completed_date: 2026-05-14
---

# Phase 3 Plan 01: Add Brainstorming and TDD Skills to test-gen

## One-liner

Added `superpowers:brainstorming` (CPI-09) and `superpowers:test-driven-development` (CPI-10) to test-gen agent frontmatter plus body references in Global Capabilities and Step 2: Design coverage.

## Tasks

### Task 1: Add brainstorming and TDD skills to test-gen frontmatter

- **Type:** auto
- **Commit:** 04c51ec9d
- **Details:** Expanded `skills:` field from 3 to 5 entries in exact specified order. Preserved `name:`, `description:`, and `tools:` fields unchanged. 2-space YAML indentation maintained.
- **Files modified:** `.claude/agents/test-gen.md`
- **Verification:** grep confirms exactly 5 entries under skills section in frontmatter.

### Task 2: Reference brainstorming and TDD skills in test-gen body workflow

- **Type:** auto
- **Commit:** 5e002d604
- **Details:** Added two integration points:
  1. Global Capabilities section: new paragraph after existing `superpowers:verification-before-completion` sentence referencing both skills
  2. Step 2: Design coverage: new text before scenario table referencing both skills for systematic enumeration and test-first methodology
- **Files modified:** `.claude/agents/test-gen.md`
- **Verification:** All 3 skill names present in body (each 3 times total = 1 frontmatter + 2 body references). All 5 workflow steps preserved. No structural changes.

## Deviations from Plan

None — plan executed exactly as written.

## Known Stubs

None — agent definition file, no renderable stubs.

## Threat Flags

None — both new skills are from official `claude-plugins-official` marketplace (T-03-01 mitigated). No new network endpoints, auth paths, file access patterns, or trust boundary crossings introduced.

## Final State

- test-gen.md frontmatter has 5 skills in correct order
- test-gen.md body references brainstorming and TDD in Global Capabilities and Step 2
- All 5 workflow steps preserved (Steps 1-5)
- No preamble changes (test-gen has zero spawn targets per D-05)
- No new tools or frontmatter field changes

## Self-Check: PASSED

- File exists: `/home/rimuru/Projects/Code/homework/OS/tgoskits/.claude/agents/test-gen.md` — FOUND
- Commit 04c51ec9d exists — FOUND
- Commit 5e002d604 exists — FOUND
- Frontmatter skills: 5 — PASS
- Brainstorming references: 3 (>= 2) — PASS
- TDD references: 3 (>= 2) — PASS
- All 5 workflow Steps present — PASS
- 0 untracked files — PASS
