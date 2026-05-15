---
name: self-evolve
description: Cyclic self-review and improvement — audit all plugin files, find issues, fix, validate, repeat
args:
  - name: rounds
    type: integer
    required: false
    default: 5
---

# /self-evolve — Plugin Self-Evolution

Cyclically audit and improve the TGOSKits Claude Code plugin. Each round scans all files under `.claude/` (excluding cache and skills), finds issues across 7 audit dimensions, fixes them, and validates before the next round.

## Input

- `/self-evolve` → default 5 rounds
- `/self-evolve 3` → 3 rounds

## Execution

Read `.claude/agents/self-evolve.md` and follow its full workflow:

1. **Audit** — Read all plugin files. Check against 7 dimensions: path references, syntax validity, frontmatter consistency, cross-reference consistency, command/flag correctness, logical completeness, hook integration.
2. **Classify** — Assign severity (BLOCK/WARN/INFO) and dimension (D1–D7) to each finding.
3. **Fix** — Fix BLOCK items first, then WARN. Minimal targeted changes.
4. **Validate** — Run JSON/Python/Bash syntax checks after all fixes.
5. **Report** — Summarize round results, proceed to next round.

After all rounds: produce a final report with round-by-round breakdown and any deferred items.

## Rules

- Read EVERY file in scope each round (no memory from prior rounds)
- Fix BLOCK items immediately; apply one fix per edit
- Do NOT modify `.claude/skills/` or `.claude/cache/`
- Validate syntax after every batch of fixes
- If a WARN persists 2+ rounds without a clear fix, defer it
