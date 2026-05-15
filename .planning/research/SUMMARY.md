# Research Summary: TGOSKits Plugin Enhancement

## Key Findings

### Stack

Claude Code builds a unified namespace at session start by scanning all installed plugins (`~/.claude/plugins/cache/*/`) and the project-local `.claude/` directory, merging skills and agents into a single registry keyed by `plugin-name:skill-name` (e.g., `superpowers:verification-before-completion`). Cross-plugin skill references in agent frontmatter use this colon-delimited format to resolve skills from external plugins; agent delegation follows the same merged-registry pattern. However, no version dependency mechanism exists -- if a referenced plugin is uninstalled, skill references silently fail with no load-time error, and all 6 TGOSKits agents already hard-depend on `superpowers:verification-before-completion`.

### Integration Patterns

Three patterns govern how TGOSKits agents compose with installed plugins:

| Pattern | Mechanism | Use When | TGOSKits Application |
|---------|-----------|----------|---------------------|
| **Skill-referencing** | `skills:` frontmatter + `Skill` tool invocation | Loading process methodology, domain knowledge, or behavioral constraints into agent context | All agents load `superpowers:verification-before-completion` as the universal completion gate; `bug-hunt` loads `superpowers:systematic-debugging` for debugging methodology |
| **Agent-delegation** | Spawn subagent via `Task` tool | Offloading bounded sub-tasks with clear I/O contracts that need isolated context or different expertise | `impl` spawns `test-gen` for test creation; `pr-review`/`bug-hunt`/`impl`/`driver-audit` all spawn `security-auditor` for safety review |
| **Command-orchestration** | Slash command loads skills then follows agent workflow | User-facing entry points that chain skills and agents into a structured pipeline | `/self-evolve` reads the `self-evolve` agent, then runs audit-fix-validate cycles |

Critical distinction: **skills are for methodology ("how to think"), agents are for deliverables ("go produce this").** Never read an agent's `.md` file as a skill substitute -- spawn the agent instead. Maximum 2 levels of agent nesting to avoid context explosion.

### Per-Agent Integration Plan

| Agent | Skills to Add | Agents to Spawn | Tools to Add |
|-------|--------------|----------------|-------------|
| `pr-review` | `superpowers:systematic-debugging`, `superpowers:receiving-code-review` | `pr-review-toolkit:code-reviewer` (pre-filter), `pr-review-toolkit:pr-test-analyzer` (non-concurrency PRs), `pr-review-toolkit:silent-failure-hunter` (error-handling PRs) | `WebSearch`, `WebFetch` |
| `bug-hunt` | `superpowers:test-driven-development`, `superpowers:dispatching-parallel-agents` | `pr-review-toolkit:silent-failure-hunter` (error-handling bugs) | `WebSearch`, `WebFetch` |
| `impl` | `superpowers:systematic-debugging` | `pr-review-toolkit:type-design-analyzer` (new types), `pr-review-toolkit:code-simplifier` (post-impl polish) | None (already has WebSearch/WebFetch) |
| `test-gen` | `superpowers:brainstorming`, `superpowers:test-driven-development` | None (pure specialist) | None |
| `driver-audit` | `superpowers:systematic-debugging` | `pr-review-toolkit:type-design-analyzer` (trait design) | None |
| `self-evolve` | `superpowers:brainstorming`, `superpowers:systematic-debugging`, `plugin-dev:skill-development`, `plugin-dev:agent-development`, `plugin-dev:hook-development` | `plugin-dev:plugin-validator` (D2/D3 automation) | None |

### Critical Risks

Ranked by severity:

| # | Risk | Severity | Prevention |
|---|------|----------|-----------|
| 1 | **Silent cross-plugin skill resolution failure** -- uninstalling `superpowers` breaks all 6 agents' `verification-before-completion` gate with no error at session start | **HIGH** | Pin superpowers version in project dependency manifest; add self-evolve D4 check that validates all `plugin:skill` references resolve against `installed_plugins.json`; document the hard dependency in CLAUDE.md |
| 2 | **Agent delegation with no safety net** -- `security-auditor` is spawned by 4 agents but is not declared as a dependency anywhere; if missing, security review is silently skipped | **HIGH** | Document required global agents in CLAUDE.md; add startup validation to each agent that checks spawnable agents exist; fail loudly ("ABORTED: security-auditor not available") rather than silently skipping |
| 3 | **Cross-plugin instruction priority conflicts** -- 4 user rule files (QWEN.md, CLAUDE.md, GEMINI.md, OPENCODE.md) compete with TGOSKits agent rules and superpowers skills; Socratic Gate mandates 3 questions before any code but `self-evolve` says "fix ALL BLOCK items first" | **HIGH** | Add explicit rule priority declaration in each agent body; test agents under various CLAUDE.md configurations; add conflict resolution section to project CLAUDE.md |
| 4 | **Version drift undetectable** -- 11 of 19 installed plugins show version `"unknown"`; if superpowers 6.0 renames `verification-before-completion`, every agent silently degrades with no warning until invocation | **MEDIUM** | Maintain project-level plugin snapshot (SHA256 of all plugin files); self-evolve should periodically verify cross-plugin references against current install state |
| 5 | **Name collision risk** -- `impl`, `test`, and `pr-review` are generic names that could collide with agents/commands from other installed plugins | **MEDIUM** | Consider namespace prefix (`tg-impl`, `tg-pr-review`); add collision detection to self-evolve D4 that scans global plugins for name conflicts |

### Recommended Phase Structure

Based on dependency chains and risk prioritization, the work should be split into three phases:

**Phase 1: Foundation (Risk Mitigation)**
- Document required global agents and superpowers dependency in CLAUDE.md (Pitfalls 9, 1)
- Add missing-agent detection with loud failures to all agents that spawn subagents (Pitfall 3)
- Add `WebSearch` + `WebFetch` to `pr-review` and `bug-hunt` agent frontmatter (Architecture gap)
- Add `superpowers:systematic-debugging` to `pr-review` frontmatter (FEATURES P0)

**Phase 2: self-evolve Enhancement**
- Add `plugin-dev:plugin-validator` spawn to automate D2 (Syntax) + D3 (Frontmatter) checks (FEATURES P0)
- Extend D4 cross-reference check to validate against `installed_plugins.json` (Pitfall 1)
- Add `plugin-dev:skill-development`, `agent-development`, `hook-development` skills for D3/D7 enhancement (FEATURES P1)
- Add optional agent-name collision detection against global plugins (Pitfall 2)
- Integrate plugin version snapshot validation (Pitfall 6)

**Phase 3: Cross-Plugin Agent Integration**
- Integrate `pr-review-toolkit:code-reviewer` as pre-filter in pr-review workflow (FEATURES P2)
- Add code-review's eligibility check pattern (skip closed/draft PRs) to pr-review (FEATURES P1)
- Add confidence scoring (0-100) as secondary axis alongside BLOCK/WARN/INFO (FEATURES P1)
- Extend bug-hunt and impl with appropriate pr-review-toolkit agents (FEATURES P2-P3)
- Add remaining superpowers skill assignments to test-gen, self-evolve, driver-audit (FEATURES P3)
