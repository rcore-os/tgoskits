# TGOSKits Plugin Enhancement

## What This Is

Enhance the TGOSKits Claude Code plugin (`.claude/`) by deeply integrating installed Claude Code plugins — superpowers, pr-review-toolkit, code-review, and plugin-dev — into all 6 custom agents. Each agent gains both skill references (frontmatter-level guidance) and agent delegation (sub-agent spawning for heavy analysis). The result: OS/kernel development workflows that leverage battle-tested general-purpose plugins without reinventing them.

## Core Value

TGOSKits agents deliver higher-quality OS development output by composing specialized installed plugins rather than duplicating their logic. Each agent focuses on its OS-domain expertise while delegating general software engineering concerns (code review, debugging methodology, TDD, plugin quality) to the plugins that already excel at them.

## Requirements

### Validated

- ✓ TGOSKits plugin with 6 agents (pr-review, bug-hunt, impl, self-evolve, test-gen, driver-audit) — existing
- ✓ 4 slash commands (test, pr-prep, impl, self-evolve) — existing
- ✓ Hooks for pre-PR gate and tool-use logging — existing
- ✓ 6 project-local skills (arceos-test-adapter, board-uboot-fsck-repair, cross-kernel-driver, review-open-prs, starry-test-suit, update-std-tests) — existing
- ✓ Already integrated: superpowers:verification-before-completion (all agents), superpowers:systematic-debugging (bug-hunt) — existing
- ✓ Published to https://github.com/seek-hope/tgoskits-plugin — existing

### Active

- [ ] **AGENT-01**: pr-review agent uses pr-review-toolkit sub-agents (code-reviewer, silent-failure-hunter, pr-test-analyzer) for deep PR analysis
- [ ] **AGENT-02**: pr-review agent loads code-review:code-review skill for review methodology guidance
- [ ] **AGENT-03**: bug-hunt agent uses superpowers:test-driven-development for repro-test-before-fix workflow
- [ ] **AGENT-04**: impl agent uses superpowers:brainstorming for feature planning before implementation
- [ ] **AGENT-05**: impl agent uses superpowers:executing-plans for structured implementation with review checkpoints
- [ ] **AGENT-06**: self-evolve agent uses plugin-dev:agent-development for agent quality audit
- [ ] **AGENT-07**: self-evolve agent uses plugin-dev:skill-development for skill quality review
- [ ] **AGENT-08**: test-gen agent uses superpowers:test-driven-development for test-first methodology
- [ ] **AGENT-09**: driver-audit agent verified — current skill set adequate, no gaps
- [ ] **AGENT-10**: All agent descriptions updated to reflect new capabilities and trigger conditions

### Out of Scope

- Creating new agents beyond the existing 6 — focus is enhancing what exists
- Changing the plugin's commands or hooks — focus is agent enhancement only
- Modifying the installed plugins themselves — consume only, don't fork
- Workflow orchestration (chaining agents into pipelines) — may follow as separate project

## Context

TGOSKits is a monorepo for OS development (ArceOS unikernel, StarryOS teaching OS, Axvisor hypervisor). The `.claude/` directory contains a project-local Claude Code plugin with custom agents tailored for kernel development: PR review against POSIX/Linux semantics, bug hunting with Linux behavior comparison, systematic feature implementation, and driver architecture auditing.

The user has 28 Claude Code plugins installed globally, including superpowers (brainstorming, debugging, TDD, verification), pr-review-toolkit (specialized PR review sub-agents), code-review (code review commands), and plugin-dev (plugin/agent/skill development tooling).

The integration pattern: Claude Code agents can reference skills from installed plugins via `skills:` frontmatter, and can spawn installed plugin agents as sub-agents via the Agent tool. The TGOSKits agents already demonstrate this pattern with superpowers skills.

## Constraints

- **Plugin boundary**: TGOSKits plugin must remain self-contained in `.claude/` — reference installed plugins, don't bundle them
- **Compatibility**: Must work with the installed versions of superpowers (5.1.0), pr-review-toolkit, code-review, plugin-dev
- **Git scope**: Agent changes belong on dev branch and in the plugin repo (github.com/seek-hope/tgoskits-plugin), not in feature PRs
- **No regression**: All existing agent functionality must be preserved — enhancements are additive

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Both integration styles (skills + delegation) | Skills provide lightweight guidance; agent delegation handles heavy analysis. Using both maximizes benefit from installed plugins. | — Pending |
| Enhance all 6 agents, not subset | Each agent has a natural installed-plugin counterpart. Partial enhancement creates inconsistency. | — Pending |
| plugin-dev for self-evolve | self-evolve audits plugin quality; plugin-dev provides authoritative agent/skill development patterns | — Pending |

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition** (via `/gsd-transition`):
1. Requirements invalidated? → Move to Out of Scope with reason
2. Requirements validated? → Move to Validated with phase reference
3. New requirements emerged? → Add to Active
4. Decisions to log? → Add to Key Decisions
5. "What This Is" still accurate? → Update if drifted

**After each milestone** (via `/gsd-complete-milestone`):
1. Full review of all sections
2. Core Value check — still the right priority?
3. Audit Out of Scope — reasons still valid?
4. Update Context with current state

---
*Last updated: 2026-05-13 after initialization*
