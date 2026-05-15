# FEATURES — Claude Code Plugin Integration Patterns for TGOSKits

**Domain**: Plugin composition, capability delegation, and integration architecture
**Date**: 2026-05-13
**Status**: Complete

---

## 1. Installed Plugin Capability Inventory

### 1.1 pr-review-toolkit (unknown version)

| Component | Name | Type | Description |
|-----------|------|------|-------------|
| Command | `review-pr` | command | Orchestrates multi-agent PR review; accepts `[review-aspects]` arg; sequential or parallel |
| Agent | `code-reviewer` | agent | CLAUDE.md compliance + bug detection; confidence scoring 0-100; only reports issues >= 80 |
| Agent | `code-simplifier` | agent | Simplifies code for clarity while preserving functionality; operates on recent changes |
| Agent | `comment-analyzer` | agent | Verifies comment accuracy vs code; identifies comment rot; suggests removals/improvements |
| Agent | `pr-test-analyzer` | agent | Behavioral test coverage analysis; rates gaps 1-10; focuses on criticality over line coverage |
| Agent | `silent-failure-hunter` | agent | Audits error handling; empty catch blocks, broad catches, unjustified fallbacks, hidden errors |
| Agent | `type-design-analyzer` | agent | Rates type design on encapsulation, invariant expression, usefulness, enforcement (each 1-10) |

### 1.2 code-review (unknown version)

| Component | Name | Type | Description |
|-----------|------|------|-------------|
| Command | `code-review` | command | Automated PR review: 5 parallel agents (CLAUDE.md, bugs, git blame, prior PR comments, comment compliance) with 0-100 confidence scoring, filters at >= 80 |

### 1.3 superpowers (5.1.0)

| Component | Name | Type | Description |
|-----------|------|------|-------------|
| Skill | `verification-before-completion` | skill | Iron law: NO completion claims without fresh verification evidence. Gate function: IDENTIFY, RUN, READ, VERIFY, only then claim. |
| Skill | `brainstorming` | skill | HARD-GATE before any creative work. Explore context, ask clarifying Qs, propose 2-3 approaches, present design, get approval. 9-step checklist. |
| Skill | `systematic-debugging` | skill | Iron law: NO FIXES WITHOUT ROOT CAUSE INVESTIGATION FIRST. 4 phases: investigation, reproduction, fix, verification. |
| Skill | `test-driven-development` | skill | Iron law: NO PRODUCTION CODE WITHOUT A FAILING TEST FIRST. Red-Green-Refactor cycle. Delete any code written before test. |
| Skill | `writing-plans` | skill | Write comprehensive plans assuming zero-context engineer. Bite-sized tasks (2-5 min each). |
| Skill | `subagent-driven-development` | skill | Fresh subagent per task + two-stage review (spec then quality). Continuous execution without pausing. |
| Skill | `executing-plans` | skill | Load plan, review critically, execute tasks, verify. Prefer subagent-driven-development if subagents available. |
| Skill | `dispatching-parallel-agents` | skill | One agent per independent problem domain. Use when 3+ unrelated failures. |
| Skill | `using-git-worktrees` | skill | Isolate workspace. Prefer native tools (EnterWorktree), fallback to git worktree. Already-detected isolation is respected. |
| Skill | `finishing-a-development-branch` | skill | Verify tests, detect environment, present options (merge/PR/cleanup), execute choice. |
| Skill | `receiving-code-review` | skill | Technical evaluation of feedback, not performative agreement. Verify before implementing. Push back with reasoning if wrong. |
| Skill | `requesting-code-review` | skill | Dispatch code reviewer subagent. Review early, review often. |
| Skill | `using-superpowers` | skill | How to find and use skills. Must invoke Skill tool before ANY response. |
| Skill | `writing-skills` | skill | TDD applied to skill creation. RED-GREEN-REFACTOR for documentation. |

### 1.4 plugin-dev (unknown version — latest)

| Component | Name | Type | Description |
|-----------|------|------|-------------|
| Command | `create-plugin` | command | 8-phase end-to-end plugin creation workflow (Discover → Plan → Design → Structure → Implement → Validate → Test → Document) |
| Agent | `agent-creator` | agent | Generates agent .md files from requirements: identifier, description, examples, system prompt, model, color, tools |
| Agent | `plugin-validator` | agent | Comprehensive plugin validation: manifest, structure, commands, agents, skills, hooks, MCP, security |
| Agent | `skill-reviewer` | agent | Reviews skill quality: description triggering, progressive disclosure, writing style, word count |
| Skill | `plugin-structure` | skill | Plugin organization, manifest (plugin.json), auto-discovery, naming conventions |
| Skill | `skill-development` | skill | Creating skills with progressive disclosure (core SKILL.md + references/ + examples/ + scripts/) |
| Skill | `agent-development` | skill | Creating autonomous agents with AI-assisted generation, frontmatter, triggering conditions, model selection |
| Skill | `command-development` | skill | Legacy commands/ format; prefers skills/ for new plugins |
| Skill | `hook-development` | skill | Advanced hooks API: PreToolUse, PostToolUse, Stop, etc. Prompt-based vs command-based hooks. |
| Skill | `mcp-integration` | skill | MCP server integration: SSE, stdio, HTTP, WebSocket. Resource and tool setup. |
| Skill | `plugin-settings` | skill | Per-project configuration via `.claude/plugin-name.local.md` with YAML frontmatter |

---

## 2. Integration Patterns

### 2.1 Pattern Taxonomy

| Pattern | Mechanism | When to Use | Example |
|---------|-----------|-------------|---------|
| **Skill-referencing** | `skills:` in agent frontmatter; `Skill` tool call at runtime | Loading domain knowledge, behavioral constraints, or process methodology into the agent's context | `bug-hunt` loads `superpowers:systematic-debugging` |
| **Agent-delegation** | `Task` tool (spawn subagent) | Offloading independent work that needs isolated context, different model, or specialized expertise | `impl` spawns `test-gen` agent for test creation |
| **Command-orchestration** | Slash command that calls Skill tool, then follows agent .md | User-facing entry point that loads skills and delegates to agents | `/self-evolve` reads `self-evolve` agent, spawns validation agents |
| **Direct tool-call** | Agent uses `Read`/`Bash`/`Grep` directly | Simple lookup or execution that doesn't need skill context or agent delegation | `syscall-diff.py` invocation in `impl` agent |
| **Plugin cross-reference** | Agent lists skills from other plugins via namespace prefix (`plugin:skill`) | Loading capabilities from external plugins without coupling | `superpowers:verification-before-completion` |

### 2.2 Skill-Referencing vs Agent-Delegation Decision Table

| Factor | Use Skill-Referencing | Use Agent-Delegation |
|--------|----------------------|---------------------|
| **Context isolation** | No — knowledge loaded into parent context | Yes — fresh isolated context, no pollution |
| **Parallelization** | No — sequential | Yes — multiple agents concurrently |
| **Model selection** | Uses parent model | Can specify different model (opus/sonnet/haiku) |
| **Cost** | Low (context tokens only) | High (new session + context + tool calls) |
| **Idempotency** | Read-only influence on behavior | Produces side effects (writes files, creates PRs) |
| **When expertise is** | A methodology or checklist to follow | A deliverable to produce |
| **Good for** | "How should I think about this?" | "Go do this and come back with results" |

**TGOSKits application**: Agents use **skill-referencing** for process skills (verification-before-completion, systematic-debugging) to enforce methodology. They use **agent-delegation** for producing deliverables (test-gen creates test files, security-auditor produces audit reports).

### 2.3 The Composition Hierarchy

```
User invokes slash command (e.g., /bug-hunt)
  → Command loads skills into context
    → Agent follows its .md workflow (process gate: systematic-debugging)
      → Agent spawns sub-agents for independent work (agent-delegation)
        → Sub-agents load their own skills (skill-referencing)
      → Agent validates with skill (verification-before-completion)
    → Agent reports result
```

---

## 3. What pr-review-toolkit Offers TGOSKits pr-review

### 3.1 Capability Map

| pr-review-toolkit capability | TGOSKits pr-review equivalent | Gap / Enhancement |
|------------------------------|-------------------------------|-------------------|
| `code-reviewer` — CLAUDE.md compliance check | D1-D7 audit dimensions with BLOCK/WARN/INFO | pr-review-toolkit's agent is more general-purpose; TGOSKits is kernel-specific |
| `code-simplifier` — clarify without changing behavior | Not present in pr-review agent | **Gap**: TGOSKits pr-review could delegate simplification post-review |
| `comment-analyzer` — comment accuracy check | Not present | **Gap**: No comment audit in TGOSKits pr-review |
| `pr-test-analyzer` — test coverage quality | Test Validity Audit (concurrency only) | TGOSKits has deeper test validation for concurrency but lacks general coverage analysis |
| `silent-failure-hunter` — error handling audit | Safety Checklist (6 items) + Sync Boundary Audit | TGOSKits is more comprehensive for kernel patterns; pr-review-toolkit's agent is application-focused |
| `type-design-analyzer` — type invariants | Not present | **Gap**: No type design review in TGOSKits; relevant for driver trait design |

### 3.2 Specific Recommendations

| TGOSKits pr-review should use... | For... | How |
|----------------------------------|--------|-----|
| `code-reviewer` agent | General code quality pass before kernel-specific review | Spawn as subagent before Step 2 (Per-file review), then supplement with kernel-specific dimensions |
| `pr-test-analyzer` agent | General test coverage gaps | Add to review workflow for non-concurrency PRs; TGOSKits Test Validity Audit handles concurrency tests |
| `code-simplifier` agent | Post-fix polish | After auto-fixing BLOCK items (Step 4), spawn `code-simplifier` on touched files |
| `comment-analyzer` agent | Documentation PRs | Auto-detect when PR adds doc comments and spawn comment-analyzer |

### 3.3 What pr-review-toolkit Should NOT Replace

TGOSKits pr-review has unique kernel-specific review dimensions that no generic plugin provides:

- **Syscall semantics verification** (return value + errno against Linux man-pages)
- **Synchronization Boundary Audit** (enumerate access sites, map primitives, verify shared boundary)
- **Test Validity Audit** (red-green verification for concurrency fixes, QEMU stability checks)
- **Kernel precondition verification** (futex across CLONE_VM, futex across fork, etc.)
- **Bug taxonomy** (Root Cause x Manifestation matrix with concurrency subtypes)
- **Layer violation checks** (kernel using ulib types)
- **invalid-test / silent-bug** detection for concurrency fixes

These are TGOSKits' **differentiating capabilities**. The pr-review-toolkit agents are supplementary, not replacements.

---

## 4. What code-review Offers TGOSKits pr-review

### 4.1 Capability Map

| code-review feature | TGOSKits pr-review equivalent | Analysis |
|--------------------|-------------------------------|----------|
| **Parallel multi-agent review** (5 agents) | Sequential single-agent review | code-review's parallel pattern is more thorough for general quality; TGOSKits pr-review is deeper but narrower |
| **Confidence-based scoring** (0-100) | BLOCK/WARN/INFO triage | code-review's numeric scoring with >=80 threshold reduces noise; TGOSKits could adopt this as secondary axis |
| **Git blame/history analysis** (Agent 3) | Not present | **Gap**: TGOSKits doesn't check historical context of modified code |
| **Previous PR comment cross-reference** (Agent 4) | Not present | **Gap**: TGOSKits doesn't check if prior PR feedback applies |
| **Code comment compliance** (Agent 5) | Not present | **Gap**: TGOSKits doesn't check code comments for guidance |
| **Eligibility check** | Not present | **Gap**: TGOSKits doesn't skip closed/draft/already-reviewed PRs |
| **Auto-skip** trivial PRs | Not present | **Gap**: Useful pattern to adopt |

### 4.2 Specific Recommendations

| TGOSKits pr-review should adopt... | Priority | Rationale |
|------------------------------------|----------|-----------|
| **Eligibility check before review** | HIGH | Skip closed, draft, already-reviewed PRs before spending review time |
| **Confidence scoring as secondary axis** | MEDIUM | Add confidence (0-100) alongside BLOCK/WARN/INFO; only report >=80 |
| **Git blame/history subagent** | MEDIUM | Spawn agent to check if modified code has historical bug patterns |
| **Previous PR comment cross-reference** | LOW | Only for files with recent prior PRs; not always applicable in kernel context |

### 4.3 What TGOSKits Should NOT Adopt

- **Full parallel multi-agent pattern for every PR**: Kernel review requires deep sequential analysis of shared state; parallel agents miss cross-file synchronization issues
- **code-review's narrow false-positive definition**: code-review treats "general code quality issues" as false positives unless in CLAUDE.md; TGOSKits kernel code requires broader quality checks
- **"Don't check build signal" rule**: kernel code must compile; TGOSKits should always verify compilation

---

## 5. Superpowers Skills Beyond verification-before-completion

### 5.1 Current TGOSKits Agent Skill Assignments

| Agent | Current skills | Adequate? |
|-------|---------------|-----------|
| `pr-review` | `review-open-prs`, `starry-test-suit`, `arceos-test-adapter`, `superpowers:verification-before-completion` | Partial |
| `bug-hunt` | `starry-test-suit`, `cross-kernel-driver`, `arceos-test-adapter`, `superpowers:systematic-debugging`, `superpowers:verification-before-completion` | Good |
| `self-evolve` | `superpowers:verification-before-completion` | Under-provisioned |
| `driver-audit` | `cross-kernel-driver`, `superpowers:verification-before-completion` | Partial |
| `test-gen` | `starry-test-suit`, `arceos-test-adapter`, `superpowers:verification-before-completion` | Partial |
| `impl` | `starry-test-suit`, `arceos-test-adapter`, `superpowers:verification-before-completion` | Good for current purpose |

### 5.2 Recommended Additional Skill Assignments

| Agent | Should add | Rationale |
|-------|-----------|-----------|
| `pr-review` | `superpowers:systematic-debugging` | When reviewing a bug fix, must verify root cause is actually addressed |
| `pr-review` | `superpowers:receiving-code-review` | When its own review is challenged — needs technical pushback pattern, not performative agreement |
| `bug-hunt` | `superpowers:test-driven-development` | Already writes repro tests; this formalizes RED-GREEN-REFACTOR |
| `bug-hunt` | `superpowers:dispatching-parallel-agents` | When hunting multiple independent bugs, dispatch parallel hunters |
| `self-evolve` | `superpowers:brainstorming` | Before changing plugin structure, explore alternatives |
| `self-evolve` | `superpowers:systematic-debugging` | When self-evolve finds issues it can't fix, debug systematically |
| `driver-audit` | `superpowers:systematic-debugging` | When layering violations have unclear root cause |
| `test-gen` | `superpowers:brainstorming` | Before designing complex test scenarios, explore edge cases |
| `test-gen` | `superpowers:test-driven-development` | Tests generated with TDD methodology produce better coverage |
| `impl` | `superpowers:systematic-debugging` | When implementation mismatches Linux behavior, debug systematically |

### 5.3 Skills Deliberately NOT Recommended

| Skill | Why NOT for TGOSKits agents |
|-------|------------------------------|
| `superpowers:subagent-driven-development` | TGOSKits agents are themselves the subagents; recursive subagent spawning without careful design creates context explosion |
| `superpowers:executing-plans` | TGOSKits has its own plan format (.planning/); this skill assumes `docs/superpowers/plans/` convention |
| `superpowers:finishing-a-development-branch` | TGOSKits agents use cherry-pick workflow for clean PR branches; finishing-a-development-branch assumes different workflow |
| `superpowers:requesting-code-review` | TGOSKits has its own PR process with pr-review agent; generic reviewer would lack kernel context |
| `superpowers:using-git-worktrees` | Platform-level concern; agents should not manage worktrees — that's the orchestrator's job |
| `superpowers:writing-skills` | TGOSKits skills are maintained separately from agents; this skill is for skill authors, not agent runtime |
| `superpowers:using-superpowers` | Meta-skill — should only fire at session start, not during agent execution |

---

## 6. What plugin-dev Offers for self-evolve

### 6.1 Current self-evolve Agent

The self-evolve agent audits TGOSKits plugin files across 7 dimensions (D1-D7):
- D1: Path Reference Correctness
- D2: Syntax & Validity (JSON, Python, Bash)
- D3: Frontmatter Consistency
- D4: Cross-Reference Consistency
- D5: Command/Flag Correctness
- D6: Logical Completeness
- D7: Hook Integration

It runs iterative rounds: audit → classify → fix → validate → repeat.

### 6.2 plugin-dev Capabilities Relevant to self-evolve

| plugin-dev component | Relevance to self-evolve | How to integrate |
|---------------------|-------------------------|-----------------|
| `plugin-validator` agent | **HIGH** — Automates D2+D3+D4 checks with structured report | Spawn `plugin-validator` as Step 1 of each round; use its report as baseline, then supplement with D5+D6+D7 |
| `skill-reviewer` agent | **MEDIUM** — self-evolve excludes `.claude/skills/` but could optionally audit skill quality | Add optional `--with-skills` flag to `/self-evolve` |
| `agent-creator` agent | **LOW** — Not relevant; self-evolve fixes existing agents, doesn't create new ones | N/A |
| `skill-development` skill | **MEDIUM** — self-evolve could verify skill best practices (progressive disclosure, description quality) | Load `plugin-dev:skill-development` when extending self-evolve scope |
| `agent-development` skill | **MEDIUM** — self-evolve could verify agent best practices (frontmatter completeness, model selection, tool selection) | Load `plugin-dev:agent-development` for D3 enhancement |
| `plugin-structure` skill | **LOW** — self-evolve only cares about manifest correctness; structure is fixed | N/A |
| `hook-development` skill | **MEDIUM** — D7 currently checks integration; could use hook-development knowledge to validate hook patterns | Load `plugin-dev:hook-development` for D7 enhancement |
| `create-plugin` command | **LOW** — Not relevant; self-evolve audits existing plugin, doesn't create new ones | N/A |

### 6.3 Specific Recommendations for self-evolve

| Priority | Change | Implementation |
|----------|--------|---------------|
| **HIGH** | Add `plugin-validator` spawn in D2/D3 | Replace manual JSON/Python/Bash syntax checks with `plugin-validator` subagent call; it checks everything D2+D3 covers plus more |
| **HIGH** | Add `plugin-dev:skill-development` to skills | When self-evolve audits commands (which follow skill patterns), apply skill quality standards |
| **MEDIUM** | Add `plugin-dev:agent-development` to skills | Apply agent best-practice patterns during D3 (model selection, color assignment, tool restrictions) |
| **MEDIUM** | Add `plugin-dev:hook-development` to skills | Enhance D7 with hook pattern validation from hook-development skill |
| **LOW** | Add optional skill review via `skill-reviewer` | Add `--with-skills` flag to audit `.claude/skills/` quality using the skill-reviewer agent |

### 6.4 What self-evolve Should NOT Adopt from plugin-dev

- **`create-plugin` command workflow**: self-evolve audits an existing plugin, not creating a new one
- **`agent-creator` agent**: self-evolve fixes existing agents; creation is a different domain
- **`mcp-integration` skill**: TGOSKits plugin doesn't use MCP servers currently
- **`plugin-settings` skill**: TGOSKits uses `.claude/settings.json` not `.local.md`

---

## 7. Anti-Features (Things to Deliberately NOT Do)

### 7.1 Integration Anti-Patterns

| Anti-Pattern | Why it's harmful | What to do instead |
|-------------|------------------|--------------------|
| **Agent-ception** (agents spawning agents spawning agents) | Context explosion, cost multiplication, each layer loses signal | Maximum 2 levels: orchestrator → specialist agent → done |
| **Loading all superpowers skills on every agent** | Context pollution; skills compete and create conflicting instructions | Load minimum necessary skills; prioritize in frontmatter `skills:` |
| **Replacing TGOSKits kernel expertise with generic review agents** | Generic agents don't understand kernel patterns (synchronization boundaries, layer violations, syscall semantics) | Use generic agents as pre-filters, not replacements |
| **Using code-review's PR posting directly** | code-review posts GitHub comments with specific format; TGOSKits needs kernel-specific taxonomy | Use code-review's confidence scoring pattern, not its output format |
| **Blindly adopting parallel-agent-everywhere pattern** | Kernel review requires sequential analysis of shared state; parallel agents miss cross-file sync issues | Parallelize only independent domains (different subsystems, different review dimensions) |
| **Plugin circular dependencies** | If plugin A requires plugin B and B requires A, neither loads reliably | Keep TGOSKits skills self-contained; reference external skills only via `skills:` frontmatter |
| **Skill-referencing for deliverables** | Loading a skill just to get instructions on how to produce something is wrong — that's what agents are for | Skill-referencing = "how to think"; Agent-delegation = "go produce this" |
| **Verification-before-completion as checkbox ritual** | Running verification without reading output violates the iron law | Always read full output, check exit code, count failures before claiming |

### 7.2 Scope Creep Boundaries

| Boundary | Rule |
|----------|------|
| **pr-review** must NOT become a generic code reviewer | Stay focused on kernel-specific dimensions (syscall semantics, sync boundary, test validity). Supplement with generic agents; don't absorb their scope. |
| **self-evolve** must NOT become a full plugin creator | Audit + fix existing files only. If a new agent/skill is needed, that's a separate workflow (`plugin-dev:create-plugin`). |
| **bug-hunt** must NOT implement missing features | Bug-hunt fixes behavior mismatches. Feature gaps go to `impl` agent. |
| **driver-audit** must NOT auto-fix driver code | Hardware testing requires manual verification. Flag violations; don't auto-patch. |
| **test-gen** must NOT become a test runner | Generate tests, validate on Linux. Running full CI and creating PRs belongs to the agent that requested the tests. |

### 7.3 Model Selection Anti-Patterns

| Anti-Pattern | Why | Fix |
|-------------|-----|-----|
| Using haiku for kernel code review | Kernel review requires deep reasoning about safety, memory models, and Linux semantics | Use sonnet/opus for review agents; haiku only for eligibility checks |
| Using opus for simple path validation | Overkill for syntax checks that bash/python -m can do in milliseconds | Use bash/python for syntax validation; opus for semantic review |
| Inheriting parent model for review subagents | Review needs different reasoning style than implementation | Explicitly set model per agent; review agents should use sonnet minimum |

---

## 8. Summary Matrix

### 8.1 TGOSKits Agent → Plugin Capability Mapping

| TGOSKits Agent | pr-review-toolkit | code-review | superpowers skills | plugin-dev |
|----------------|-------------------|-------------|---------------------|------------|
| `pr-review` | code-reviewer, pr-test-analyzer, code-simplifier, comment-analyzer | Confidence scoring (>=80), eligibility check | systematic-debugging, receiving-code-review | — |
| `bug-hunt` | silent-failure-hunter | — | test-driven-development, dispatching-parallel-agents | — |
| `self-evolve` | — | — | systematic-debugging, brainstorming | plugin-validator agent, skill-development, agent-development, hook-development |
| `driver-audit` | type-design-analyzer | — | systematic-debugging | — |
| `test-gen` | pr-test-analyzer | — | brainstorming, test-driven-development | — |
| `impl` | — | — | systematic-debugging | — |

### 8.2 Table Stakes vs Differentiators

| Category | What ALL plugins should do | What TGOSKits uniquely does |
|----------|---------------------------|---------------------------|
| **Code review** | CLAUDE.md compliance, bug detection, style | Syscall semantics verification, Synchronization Boundary Audit, Test Validity Audit for concurrency |
| **Bug hunting** | Reproduce, isolate, fix | Linux strace reference comparison, lockdep integration, concurrency subtype taxonomy |
| **Testing** | Scenario coverage, edge cases | QEMU SMP constraints, futex mechanism precondition verification |
| **Plugin quality** | Syntax validation, frontmatter check | Kernel-specific command flag correctness (cargo xtask patterns) |
| **Driver auditing** | Architecture layering | mmio-api/dma-api enforcement, IRQ contract validation |
| **Implementation** | Discover, plan, implement | Binary-mode analysis, strace-diff gap detection, stub auto-decision heuristics |

### 8.3 Migration Priority (What to Do First)

| Priority | Action | Effort | Impact |
|----------|--------|--------|--------|
| **P0** | Add `plugin-validator` spawn to self-evolve D2/D3 | Low (one subagent call) | High (automates 2 dimensions) |
| **P0** | Add `superpowers:systematic-debugging` to `pr-review` skills | Low (one line in frontmatter) | High (prevents symptom-fixing) |
| **P1** | Add code-review's eligibility check pattern to pr-review | Low (gh CLI check) | Medium (avoids wasted review time) |
| **P1** | Add confidence scoring (0-100) as secondary axis in REVIEW.md | Medium (template change) | Medium (reduces noise) |
| **P2** | Delegate to `code-reviewer` agent before kernel-specific review | Medium (subagent integration) | Medium (catches general issues) |
| **P2** | Add `superpowers:test-driven-development` to `bug-hunt` and `test-gen` | Low (frontmatter) | Medium (formalizes existing practice) |
| **P3** | Integrate `pr-test-analyzer` for non-concurrency PR test review | Medium (conditional dispatch) | Low (already have Test Validity Audit for concurrency) |
| **P3** | Add `superpowers:brainstorming` to `test-gen` and `self-evolve` | Low (frontmatter) | Low (formalizes existing exploration) |
