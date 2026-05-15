# Phase 3: Cross-Plugin Agent Integration - Research

**Researched:** 2026-05-14
**Domain:** Plugin agent composition -- skills loading and sub-agent delegation
**Confidence:** HIGH

## Summary

Phase 3 composes all 5 remaining TGOSKits agents (pr-review, bug-hunt, impl, test-gen, driver-audit) with installed plugin skills and sub-agent delegation, following the same frontmatter-only skill loading and preamble-based agent spawn patterns established in Phase 1 and Phase 2. All 5 referenced superpowers skills and all 5 referenced pr-review-toolkit sub-agents are confirmed installed and available in the expected versions. pr-review is the most complex change (6 requirements: eligibility check, confidence scoring, 3 sub-agent spawns with conditional triggers, and a skill addition), while test-gen and driver-audit are the simplest (2 requirements each, mostly frontmatter changes).

**Primary recommendation:** All 5 agents can be modified independently (no file conflicts), but should be grouped into 3 waves by complexity: Wave 1 (test-gen + driver-audit frontmatter-only), Wave 2 (bug-hunt + impl body + frontmatter), Wave 3 (pr-review -- most complex).

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| PR code quality pre-filter | pr-review agent | code-reviewer sub-agent | code-reviewer runs first as a pre-filter; pr-review performs OS-specific review on results |
| PR eligibility check | pr-review agent | -- | Runs at invocation start before any sub-agent is spawned; lightweight PR state check |
| Confidence scoring | pr-review agent | code-reviewer, pr-test-analyzer, silent-failure-hunter sub-agents | Aggregates sub-agent findings into explicit 0-100 score |
| Test coverage audit | pr-test-analyzer sub-agent | -- | Conditionally spawned by pr-review for non-concurrency PRs |
| Error handling audit | silent-failure-hunter sub-agent | -- | Conditionally spawned by pr-review or bug-hunt for error-handling code |
| TDD workflow for bug fixes | bug-hunt agent | test-driven-development skill | Skill guides RED/GREEN/REFACTOR phases within existing repro-fix-verify flow |
| Parallel repro execution | bug-hunt agent | dispatching-parallel-agents skill | Skill invoked when multiple independent repro scenarios exist |
| Implementation debugging | impl agent | systematic-debugging skill | Skill provides structured debugging process during implementation |
| Test design ideation | test-gen agent | brainstorming skill | Skill invoked before test design for systematic coverage enumeration |
| TDD for test generation | test-gen agent | test-driven-development skill | Skill guides test-first approach in test creation |
| Driver layering audit | driver-audit agent | systematic-debugging skill | Skill provides root-cause analysis for layering violations |
| Type/trait design review | type-design-analyzer sub-agent | -- | Spawned by impl for new types, by driver-audit for trait design |
| Post-implementation polish | code-simplifier sub-agent | -- | Spawned by impl after implementation is complete |
| Handling review feedback | pr-review agent | receiving-code-review skill | Skill guides structured evaluation of external review comments |

## Standard Stack

### Plugins
| Plugin | Version | Confirmed | Status |
|--------|---------|-----------|--------|
| superpowers | 5.1.0 | `[VERIFIED: installed_plugins.json]` | Installed |
| pr-review-toolkit | 1a2f18b05cf5 | `[VERIFIED: installed_plugins.json]` | Installed |

### Superpowers Skills (all in 5.1.0)
| Skill | Confirmed | Used By |
|-------|-----------|---------|
| receiving-code-review | `[VERIFIED: SKILL.md exists in 5.1.0/skills/]` | pr-review (CPI-16) |
| test-driven-development | `[VERIFIED: SKILL.md exists in 5.1.0/skills/]` | bug-hunt (CPI-06), test-gen (CPI-10) |
| dispatching-parallel-agents | `[VERIFIED: SKILL.md exists in 5.1.0/skills/]` | bug-hunt (CPI-07) |
| systematic-debugging | `[VERIFIED: SKILL.md exists in 5.1.0/skills/]` | impl (CPI-08), driver-audit (CPI-11) |
| brainstorming | `[VERIFIED: SKILL.md exists in 5.1.0/skills/]` | test-gen (CPI-09) |

### pr-review-toolkit Sub-Agents (all in 1a2f18b05cf5/agents/)
| Sub-Agent | Confirmed | Model | Spawned By |
|-----------|-----------|-------|------------|
| code-reviewer | `[VERIFIED: agents/code-reviewer.md exists]` | opus | pr-review (CPI-01) |
| pr-test-analyzer | `[VERIFIED: agents/pr-test-analyzer.md exists]` | inherit | pr-review (CPI-04) |
| silent-failure-hunter | `[VERIFIED: agents/silent-failure-hunter.md exists]` | inherit | pr-review (CPI-05), bug-hunt (CPI-12) |
| type-design-analyzer | `[VERIFIED: agents/type-design-analyzer.md exists]` | inherit | impl (CPI-13), driver-audit (CPI-15) |
| code-simplifier | `[VERIFIED: agents/code-simplifier.md exists]` | opus | impl (CPI-14) |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| pr-review-toolkit sub-agents | Manual code review | Sub-agents provide specialized context isolation and focused expertise; manual review would consume pr-review's context budget and lack specialized analysis |
| frontmatter-only skill loading | Inline Skill() invocations | Phase 01/02 pattern already established; inline invocations would be inconsistent with existing agent design and bypass preamble validation |
| Conditional sub-agent spawns | Always-spawn-all | 3 sub-agents per review would be expensive for small/trivial PRs; conditional targeting by PR characteristics is more efficient |

### Installation Notes
```bash
# Dependencies already installed -- no new installations needed for Phase 3
# Verify current state:
cat ~/.claude/plugins/installed_plugins.json | python3 -c "import sys,json; d=json.load(sys.stdin); print('pr-review-toolkit' in d['plugins'], 'superpowers' in d['plugins'])"
```

**Version verification:** Both plugins are confirmed present with versions documented above. superpowers 5.1.0 contains all 5 referenced skills. pr-review-toolkit contains all 5 referenced sub-agents (code-reviewer, pr-test-analyzer, silent-failure-hunter, type-design-analyzer, code-simplifier).

## Architecture Patterns

### Agent Integration Architecture

```
[User Request]
     |
     v
+---------------------+
| Dependency Check    |  [Phase 01 preamble - validates skills/agents/tools]
| Preamble            |
+---------------------+
     |
     v
+---------------------+
| Agent Workflow      |  [Existing workflow preserved - additive changes only]
| (body)             |
+---------------------+
     |                                +------------------------------+
     +--[skill: superpowers:X]------> | Same-context skill guidance |
     |                                +------------------------------+
     |                                +----------------------------------+
     +--[spawn: toolkit:agent]------> | Isolated sub-agent context      |
                                      | Returns structured findings     |
                                      +----------------------------------+
```

### Agent-Skill and Agent-SubAgent Mapping

```
pr-review (CPI-01..05, CPI-16)
  Frontmatter skills:  +superpowers:receiving-code-review
  Preamble Agents:     +code-reviewer, +pr-test-analyzer, +silent-failure-hunter
  Body changes:        eligibility check (pre-filter), conditional spawns, confidence scoring

bug-hunt (CPI-06, CPI-07, CPI-12)
  Frontmatter skills:  +superpowers:test-driven-development, +superpowers:dispatching-parallel-agents
  Preamble Agents:     +silent-failure-hunter
  Body changes:        TDD phases inserted into repro-fix-verify, silent-failure-hunter spawn

impl (CPI-08, CPI-13, CPI-14)
  Frontmatter skills:  +superpowers:systematic-debugging
  Preamble Agents:     +type-design-analyzer, +code-simplifier
  Body changes:        type-design-analyzer spawn for new types, code-simplifier spawn post-impl

test-gen (CPI-09, CPI-10)
  Frontmatter skills:  +superpowers:brainstorming, +superpowers:test-driven-development
  Preamble Agents:     (none)
  Body changes:        skill invocation references only (no spawn changes)

driver-audit (CPI-11, CPI-15)
  Frontmatter skills:  +superpowers:systematic-debugging
  Preamble Agents:     +type-design-analyzer
  Body changes:        type-design-analyzer spawn for trait design review
```

### Recommended Task Structure

Each agent file is independent -- no two agents share a file. The only shared dependency is `~/.claude/plugins/installed_plugins.json` for validation (read-only, already handled by Phase 01 validate-deps.py).

### Pattern 1: Frontmatter Skill Loading (matches Phase 01/02)

**What:** Add `superpowers:skill-name` to the agent's frontmatter `skills:` list.
**When to use:** For all skill requirements (CPI-06 through CPI-11, CPI-16).
**Example:**
```yaml
skills:
  - starry-test-suit
  - arceos-test-adapter
  - superpowers:verification-before-completion
  - superpowers:brainstorming          # NEW (CPI-09)
  - superpowers:test-driven-development # NEW (CPI-10)
```

### Pattern 2: Dependency Check Preamble Agent List Update (matches Phase 01/02)

**What:** Add sub-agent names to the "Agents" section of the Dependency Check preamble.
**When to use:** For all sub-agent spawn requirements (CPI-01, CPI-04, CPI-05, CPI-12, CPI-13, CPI-14, CPI-15).
**Example:**
```markdown
**Agents** (must be spawnable):
- `pr-review-toolkit:code-reviewer` — code quality pre-filter
- `pr-review-toolkit:pr-test-analyzer` — test coverage audit (conditional)
- `pr-review-toolkit:silent-failure-hunter` — error handling audit (conditional)
```

### Pattern 3: Conditional Sub-Agent Spawn (matches Phase 02 pattern)

**What:** Describe spawn conditions in body text; spawn only when conditions are met.
**When to use:** CPI-01 (always: code-reviewer as pre-filter), CPI-04 (conditional on PR diff), CPI-05 (conditional on error-handling patterns), CPI-12 (conditional on bug classification), CPI-13/CPI-15 (conditional on new types/traits), CPI-14 (post-implementation).
**Example (pr-review conditional spawn):**
```
### Sub-Agent Spawn: pr-test-analyzer

If the PR diff contains test file changes (*.test.*, *.spec.*, test-*, tests/) 
OR the PR is labeled as non-concurrency, spawn `pr-review-toolkit:pr-test-analyzer`:

> "Analyze test coverage quality and completeness for this PR diff..."

If pr-test-analyzer is unavailable, warn and perform manual test coverage analysis.
```

### Anti-Patterns to Avoid
- **Adding ALL sub-agents to preamble without conditional logic:** Some sub-agents (pr-test-analyzer, silent-failure-hunter) should only run when PR characteristics indicate they're relevant. Listing them in preamble is fine (it validates they exist), but body text must gate spawns conditionally.
- **Using inline Skill() calls instead of frontmatter:** Phase 01/02 established frontmatter-only skill loading. Skill descriptions in body text reference skills naturally by name without explicit invocation syntax.
- **Breaking the existing workflow structure:** All changes must be additive -- preserved existing workflow phases, review dimensions, and loop controls. New sections slot alongside existing ones.
- **Custom confidence score formulas:** D-02 explicitly says 0-100 score is assigned explicitly, not computed. Do not implement a mathematical formula.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| General PR code quality review | Custom review logic | pr-review-toolkit:code-reviewer | Already specialized for project guidelines compliance with 0-100 confidence scoring |
| Test coverage analysis | Manual test audit | pr-review-toolkit:pr-test-analyzer | Structured coverage mapping with criticality ratings |
| Error handling audit | Manual catch/try/Result inspection | pr-review-toolkit:silent-failure-hunter | Systematic audit of all failure patterns with severity classification |
| Type/trait design review | Manual type invariant analysis | pr-review-toolkit:type-design-analyzer | 4-axis rating (encapsulation, expression, usefulness, enforcement) |
| Post-implementation code polish | Manual simplification pass | pr-review-toolkit:code-simplifier | Project-pattern-aware simplification preserving functionality |

**Key insight:** All 5 pr-review-toolkit sub-agents are designed for isolated context execution via Task spawns. They produce structured output that the parent agent (pr-review, impl, bug-hunt, driver-audit) consumes and integrates into its final deliverable. This is the same pattern used in Phase 02 for plugin-validator.

## Common Pitfalls

### Pitfall 1: Agent-Name Collision Between Plugins
**What goes wrong:** Spawning `code-simplifier` might resolve to the standalone `code-simplifier@claude-plugins-official` plugin's agent instead of `pr-review-toolkit:code-simplifier`. Both plugins register agents named `code-simplifier`.
**Why it happens:** The standalone code-simplifier plugin (version 1.0.0) has `agents/code-simplifier.md` with `name: code-simplifier`. The pr-review-toolkit has a separate `agents/code-simplifier.md` with the same name. When spawned as bare `code-simplifier`, Claude Code might resolve to either one.
**How to avoid:** Reference sub-agents using the full prefix format `pr-review-toolkit:code-simplifier` in all spawn instructions, matching the existing pattern used in self-evolve.md (`plugin-dev:plugin-validator`). Do not use bare names for any pr-review-toolkit sub-agent spawn.
**Warning signs:** code-simplifier returns analysis that doesn't match the pr-review-toolkit README's description (e.g., focuses on JS/TS-specific patterns instead of general code quality).

### Pitfall 2: Skill Overload Degrading Context Quality
**What goes wrong:** Adding too many skills to an agent's frontmatter causes context fragmentation -- the agent has too many "personalities" to balance.
**Why it happens:** Each skill in frontmatter adds its instructions to the system prompt. An agent like bug-hunt would have 4 superpowers skills + project skills = 6+ skill references competing for attention.
**How to avoid:** Follow the assignment table in D-05 strictly. No agent gets more than 2 new skills. bug-hunt gets 2 (TDD + parallel agents), test-gen gets 2 (brainstorming + TDD), impl gets 1 (systematic-debugging), driver-audit gets 1 (systematic-debugging), pr-review gets 1 (receiving-code-review).
**Warning signs:** Agent output becomes generic or fails to follow specific skill guidance.

### Pitfall 3: Code-Reviewer Duplication with Existing Review Dimensions
**What goes wrong:** code-reviewer's general code quality analysis overlaps with pr-review's existing POSIX/Linux-specific review dimensions, producing redundant findings.
**Why it happens:** Both code-reviewer and pr-review's own review dimensions check code quality -- code-reviewer from a general perspective, pr-review from an OS-specific lens. The same issue might be found by both.
**How to avoid:** Structure code-reviewer as a pre-filter (per D-01): run it first, get its findings, then feed those into the OS-specific review. The pr-review agent should be aware of code-reviewer's output and avoid duplicating its findings in the BLOCK/WARN/INFO output. Code-reviewer findings provide context for confidence scoring.
**Warning signs:** The REVIEW.md has duplicate entries for the same issue.

### Pitfall 4: Eligibility Check Failing on Non-GitHub PRs
**What goes wrong:** The eligibility check (CPI-02) reads PR state from GitHub API or PR metadata, but the PR might not be on GitHub (local branch, GitLab, etc.).
**Why it happens:** The agent assumes GitHub API access via `gh` CLI. If `gh` is unavailable or the PR is from a non-GitHub source, the check fails.
**How to avoid:** Make the eligibility check fallible -- if `gh` is unavailable, warn and assume eligible (proceed). If PR state can't be determined, assume eligible. Only skip when PR state is definitively closed/draft/already-reviewed. This matches the Phase 01/02 graceful fallback pattern.
**Warning signs:** "`gh` command not found" errors at pr-review invocation time.

### Pitfall 5: Conditional Spawn Thresholds Too Aggressive
**What goes wrong:** silent-failure-hunter is never spawned because the "error-handling patterns" threshold is set too high, or pr-test-analyzer is always spawned because the "test changes" threshold is too low.
**Why it happens:** Thresholds are left to Claude's discretion (D-01 says "selected: conditional based on PR characteristics"). Without explicit guidance in the body text, the agent might make inconsistent decisions.
**How to avoid:** Document clear trigger heuristics in the pr-review body:
- pr-test-analyzer: spawn when diff contains `*.test.*`, `tests/`, `test_`, `*spec*` filenames OR PR is labeled `non-concurrency`
- silent-failure-hunter: spawn when diff contains `try`, `catch`, `Result`, `Option`, `unwrap`, `expect`, `?` operator, or error types
**Warning signs:** Inconsistent behavior across review sessions.

## Code Examples

### pr-review: Eligibility Check + Sub-Agent Spawn Flow (Body Section)

```markdown
### Step 0: Eligibility Check

Before any review work, verify the PR is eligible for review:

1. Check PR state via `gh pr view <number> --json state,isDraft`
2. If state is CLOSED or MERGED: skip, report "PR is closed/merged -- no review needed"
3. If isDraft is true: skip, report "PR is a draft -- no review needed until ready"
4. If already reviewed (check review comments for previous full reviews): skip, report "Already reviewed in this session"

If `gh` CLI is unavailable or PR number not determinable: assume eligible, warn, and proceed.

### Step 1: Pre-Filter — Code Quality Review

Always spawn `pr-review-toolkit:code-reviewer` as a pre-filter:

> "Review the current PR diff (`git diff upstream/dev...HEAD`) for general code quality, project guideline compliance, and bug detection. Report findings with confidence scores."

Wait for code-reviewer output. Integrate its findings into the overall review context.

### Step 2: Conditional Sub-Agent Spawns

**pr-test-analyzer (spawn if):**
- The PR diff includes test file changes: `*.test.*`, `*.spec.*`, `test-*`, `tests/`, or
- The PR is labeled as `non-concurrency`

> "Analyze test coverage quality and completeness for this PR diff. Identify critical gaps, missing edge cases, and test quality issues."

**silent-failure-hunter (spawn if):**
- The PR diff includes error-handling patterns: `try`, `catch`, `Result`, `Option`, `unwrap`, `expect`, `?` operator, or explicit error type handling

> "Audit this PR diff for silent failures, inadequate error handling, and inappropriate fallback behavior."

### Step 3: OS-Specific Review

Proceed with the existing Step 1-6 workflow (per-file review, Synchronization Boundary Audit, Test Validity Audit). Incorporate sub-agent findings into each dimension's classification.

### Confidence Assessment

After completing all review steps, assign an explicit confidence score (0-100):

| Factor | Weight | How to Assess |
|--------|--------|---------------|
| PR complexity | — | Simple (single file/function) = higher confidence; Complex (multi-module, concurrency) = lower |
| Diff size | — | < 200 lines = higher; > 1000 lines = lower (can't inspect every path) |
| Test coverage | — | Existing tests + pr-test-analyzer findings = adjust up/down |
| Sub-agent alignment | — | code-reviewer findings match manual review = higher; contradict = lower |
| Ambiguity | — | Multiple possible root causes or unclear semantics = lower |

Score interpretation: 90+ (comprehensive), 70-89 (thorough), 50-69 (moderate gaps), < 50 (shallow -- consider re-review)

**Confidence Assessment:** [0-100]
[Brief justification referencing the factors above]
```

### bug-hunt: TDD Integration into Repro-Fix-Verify

```markdown
### Phase 2: REPRO (Reproduction) — RED Phase

Apply `superpowers:test-driven-development` RED phase:

1. Write a minimal C reproducer test that captures the bug behavior
2. Run it on the target OS via QEMU to confirm it fails (RED)
3. If the test passes on the target OS, the bug is not reproduced -- revise the test

For concurrency bugs classified as error-handling issues, additionally spawn
`pr-review-toolkit:silent-failure-hunter`:

> "Analyze the error handling patterns in [affected function/file] at [line range].
> Identify silent failures, inadequate error handling, or inappropriate fallback behavior."

### Phase 3: FIX — GREEN Phase

Apply the fix. Run the repro test to confirm it now passes (GREEN).

### Phase 4: VERIFY — REFACTOR Phase

Clean up test code and fix code. Re-run the repro test to confirm it still passes.
```

### impl: Sub-Agent Spawn for New Types

```markdown
### When introducing new types or traits

After defining a new type/trait but before implementing methods, spawn
`pr-review-toolkit:type-design-analyzer`:

> "Review the new type [name] defined in [file:line]. Evaluate encapsulation,
> invariant expression, usefulness, and enforcement."

Incorporate findings before proceeding with implementation.

### Post-implementation code polish

After all implementation phases complete and before CI loop, spawn
`pr-review-toolkit:code-simplifier`:

> "Review the recently modified files in this implementation for code
> simplification opportunities. Preserve all functionality while improving clarity."

Apply refinements from code-simplifier output, then proceed to CI loop.
```

### driver-audit: Sub-Agent Spawn for Trait Review

```markdown
### E. Trait Design Review (NEW)

After checking the four layers, if the audit scope includes trait definitions
or capability interfaces, spawn `pr-review-toolkit:type-design-analyzer`:

> "Review the trait definition [trait name] in [file:line]. Evaluate
> encapsulation, invariant expression, and whether the interface is minimal and complete."

Incorporate findings into the AUDIT.md report as an additional section.
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| pr-review does all review work in a single context | pr-review delegates to specialized sub-agents (code-reviewer, pr-test-analyzer, silent-failure-hunter) | Phase 3 | Sub-agents get isolated context for focused analysis; parent agent integrates results |
| bug-hunt manual repro-fix-verify | bug-hunt guided by TDD skill with RED/GREEN/REFACTOR phases | Phase 3 | Structured TDD methodology ensures tests fail before fix attempt |
| impl manually reviews type design | impl delegates to type-design-analyzer sub-agent | Phase 3 | Systematic type invariant analysis without consuming impl context budget |
| test-gen manual test design | test-gen guided by brainstorming + TDD skills | Phase 3 | Structured coverage enumeration and test-first methodology |
| driver-audit manual layer analysis | driver-audit delegates trait review to type-design-analyzer | Phase 3 | Automated trait design analysis alongside manual layer analysis |

## Requirements Mapping

| ID | Description | Research Support |
|----|-------------|------------------|
| CPI-01 | pr-review spawns code-reviewer as pre-filter | [VERIFIED] code-reviewer agent exists. Always-spawn pattern documented in CONTEXT.md D-01. |
| CPI-02 | pr-review eligibility check | [VERIFIED] CONTEXT.md D-03 specifies pre-filter before spawn. gh CLI-based check with fallback. |
| CPI-03 | pr-review confidence scoring | [VERIFIED] CONTEXT.md D-02 specifies explicit 0-100 as secondary axis alongside BLOCK/WARN/INFO. |
| CPI-04 | pr-review spawns pr-test-analyzer | [VERIFIED] pr-test-analyzer agent exists. Conditional on test changes or non-concurrency label. |
| CPI-05 | pr-review spawns silent-failure-hunter | [VERIFIED] silent-failure-hunter agent exists. Conditional on error-handling patterns in diff. |
| CPI-06 | bug-hunt loads test-driven-development | [VERIFIED] skill exists in superpowers 5.1.0. Fits into repro-fix-verify per CONTEXT.md D-04. |
| CPI-07 | bug-hunt loads dispatching-parallel-agents | [VERIFIED] skill exists in superpowers 5.1.0. Invoked for multiple independent repro scenarios. |
| CPI-08 | impl loads systematic-debugging | [VERIFIED] skill exists in superpowers 5.1.0. Already used by pr-review and bug-hunt. |
| CPI-09 | test-gen loads brainstorming | [VERIFIED] skill exists in superpowers 5.1.0. Guides coverage enumeration before test design. |
| CPI-10 | test-gen loads test-driven-development | [VERIFIED] skill exists in superpowers 5.1.0. Guides test-first approach. |
| CPI-11 | driver-audit loads systematic-debugging | [VERIFIED] skill exists in superpowers 5.1.0. Used for layering violation root-cause analysis. |
| CPI-12 | bug-hunt spawns silent-failure-hunter | [VERIFIED] silent-failure-hunter agent exists. Spawned for error-handling classified bugs. |
| CPI-13 | impl spawns type-design-analyzer | [VERIFIED] type-design-analyzer agent exists. Spawned when new types or traits are introduced. |
| CPI-14 | impl spawns code-simplifier | [VERIFIED] code-simplifier agent exists in pr-review-toolkit (not the standalone plugin). Post-implementation polish. |
| CPI-15 | driver-audit spawns type-design-analyzer | [VERIFIED] type-design-analyzer agent exists. Spawned for trait design review during audit. |
| CPI-16 | pr-review loads receiving-code-review | [VERIFIED] skill exists in superpowers 5.1.0. Guides structured evaluation of external review feedback. |

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | The `gh` CLI is available for the pr-review eligibility check | Common Pitfalls / pr-review Code Example | Eligibility check falls back to "assume eligible" -- sub-optimal but not blocking |
| A2 | pr-review-toolkit sub-agents produce structured output that can be integrated into REVIEW.md | Architecture Patterns | Sub-agent output format may not directly map to BLOCK/WARN/INFO; may need formatting in parent agent |
| A3 | Adding receiving-code-review skill to pr-review won't conflict with its existing review workflow | Skill Loading | The skill's "verify before implementing" guidance is compatible with pr-review's existing "auto-fix BLOCK items" workflow |

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| superpowers plugin | All 5 agents (skill loading) | Yes | 5.1.0 | Phase 01 validate-deps.py catches missing |
| pr-review-toolkit plugin | pr-review, bug-hunt, impl, driver-audit (sub-agent spawns) | Yes | 1a2f18b05cf5 | Phase 01 preamble validation catches missing |
| gh CLI | pr-review eligibility check | Not checked -- assumed available | -- | Fallback: assume eligible, warn |
| code-simplifier standalone plugin | (none -- CPI-14 uses pr-review-toolkit:code-simplifier) | Yes | 1.0.0 | Not used by Phase 3; no conflict if referenced correctly |

**Missing dependencies with no fallback:** None -- all required plugins are confirmed installed.

## Validation Architecture

### Test Framework

No automated tests exist for agent `.md` files. Validation is manual:
- **Format check:** Validate YAML frontmatter parses correctly
- **Reference check:** All `plugin:skill` references resolve against installed_plugins.json
- **Agent spawn check:** All referenced sub-agents exist in the plugin cache
- **Consistency check:** No regressions in existing workflow structure

### Phase Requirements -- Test Map

| Req ID | Behavior | Test Type | Verification Method |
|--------|----------|-----------|---------------------|
| CPI-01 | code-reviewer spawn | Manual | Check pr-review.md preamble lists code-reviewer; body describes always-spawn before OS review |
| CPI-02 | Eligibility check | Manual | Check pr-review.md body has eligibility check section before any spawn |
| CPI-03 | Confidence scoring | Manual | Check pr-review.md body has Confidence Assessment section after findings |
| CPI-04 | pr-test-analyzer spawn | Manual | Check pr-review.md body has conditional spawn for test changes |
| CPI-05 | silent-failure-hunter spawn (pr-review) | Manual | Check pr-review.md body has conditional spawn for error-handling patterns |
| CPI-06 | TDD skill in bug-hunt | Manual | Check bug-hunt.md frontmatter has `superpowers:test-driven-development` |
| CPI-07 | Parallel agents skill in bug-hunt | Manual | Check bug-hunt.md frontmatter has `superpowers:dispatching-parallel-agents` |
| CPI-08 | systematic-debugging skill in impl | Manual | Check impl.md frontmatter has `superpowers:systematic-debugging` |
| CPI-09 | brainstorming skill in test-gen | Manual | Check test-gen.md frontmatter has `superpowers:brainstorming` |
| CPI-10 | TDD skill in test-gen | Manual | Check test-gen.md frontmatter has `superpowers:test-driven-development` |
| CPI-11 | systematic-debugging skill in driver-audit | Manual | Check driver-audit.md frontmatter has `superpowers:systematic-debugging` |
| CPI-12 | silent-failure-hunter spawn (bug-hunt) | Manual | Check bug-hunt.md preamble lists silent-failure-hunter; body describes conditional spawn |
| CPI-13 | type-design-analyzer spawn (impl) | Manual | Check impl.md preamble lists type-design-analyzer; body describes spawn for new types |
| CPI-14 | code-simplifier spawn (impl) | Manual | Check impl.md preamble lists code-simplifier; body describes post-impl spawn |
| CPI-15 | type-design-analyzer spawn (driver-audit) | Manual | Check driver-audit.md preamble lists type-design-analyzer; body describes spawn for trait review |
| CPI-16 | receiving-code-review skill in pr-review | Manual | Check pr-review.md frontmatter has `superpowers:receiving-code-review` |

### Verification Script

Run after all changes to validate agent files parse correctly:
```bash
# Validate YAML frontmatter parses in all 5 agents
for f in .claude/agents/pr-review.md .claude/agents/bug-hunt.md .claude/agents/impl.md .claude/agents/test-gen.md .claude/agents/driver-audit.md; do
  python3 -c "
import yaml
with open('$f') as fh:
    content = fh.read()
    parts = content.split('---', 2)
    if len(parts) >= 3:
        fm = yaml.safe_load(parts[1])
        print(f'$f: OK - skills={fm.get(\"skills\", [])}')
    else:
        print(f'$f: ERROR - no frontmatter')
  " 2>&1 || echo "$f: YAML PARSE ERROR"
done
```

## Security Domain

This phase modifies agent `.md` files only -- no executable code, no network-facing endpoints, no data handling. Security domain is not applicable. Standard `security_enforcement` ASVS review is out of scope.

However, two agentic security patterns apply:

| Pattern | Standard Mitigation |
|---------|---------------------|
| Sub-agent prompt injection | Sub-agent spawn instructions should use fixed templates, not interpolate untrusted user input. If a PR description or user message contains text that becomes part of the spawn prompt, escape or validate it. |
| Skill instruction injection | Superpowers skills define behavior patterns. If an agent loads conflicting skills (e.g., both systematic-debugging and a skill with contradictory guidance), behavior is undefined. Maintain the selective loading by agent as defined in CONTEXT.md D-05. |

## Wave Grouping Recommendation

| Wave | Agents | Requirements | Complexity | Rationale |
|------|--------|-------------|------------|-----------|
| Wave 1 | test-gen, driver-audit | CPI-09, CPI-10, CPI-11, CPI-15 | Low -- mostly frontmatter + minor body | Only agent with no sub-agent spawns (test-gen) + simplest integration (driver-audit needs 1 skill + 1 spawn reference). Both can be verified quickly. |
| Wave 2 | bug-hunt, impl | CPI-06, CPI-07, CPI-12, CPI-08, CPI-13, CPI-14 | Medium -- frontmatter + body + spawns | Both need skill additions + sub-agent spawns. bug-hunt TDD integration aligns with its existing repro-fix-verify flow. impl's 2 spawns (type-design-analyzer + code-simplifier) bracket the implementation lifecycle. |
| Wave 3 | pr-review | CPI-01, CPI-02, CPI-03, CPI-04, CPI-05, CPI-16 | High -- most complex | 6 requirements including structural additions (eligibility check, conditional spawns, confidence scoring). Largest body of new text. Best handled last after the simpler agents validate the skill/spawn patterns. |

## Sources

### Primary (HIGH confidence)
- `~/.claude/plugins/installed_plugins.json` - Authoritative plugin presence and version source
- `~/.claude/plugins/cache/claude-plugins-official/superpowers/5.1.0/skills/` - All 5 referenced skill SKILL.md files read and confirmed
- `~/.claude/plugins/cache/claude-plugins-official/pr-review-toolkit/1a2f18b05cf5/agents/` - All 5 referenced sub-agent files read and confirmed
- `.claude/agents/*.md` - All 5 target agent files read for current state analysis
- `.claude/agents/self-evolve.md` - Reference pattern for sub-agent spawns and skill loading

### Secondary (MEDIUM confidence)
- CONTEXT.md Phase 1-3 decisions - Binding implementation constraints verified against actual plugin state

### Tertiary (LOW confidence)
- (None -- all core claims are verified)

## Metadata

**Confidence breakdown:**
- Plugin availability: HIGH -- both plugins confirmed installed with required agents/skills
- Architecture patterns: HIGH -- follows established Phase 01/02 patterns (frontmatter skills, preamble agents, additive changes)
- Agent-specific integration: HIGH -- each agent's current state was read; required changes are well-understood
- Pitfalls: MEDIUM -- edge cases around gh CLI availability and agent-name collision are based on analysis, not observed failures

**Research date:** 2026-05-14
**Valid until:** 2026-06-14 (30 days -- plugin versions and agent files are stable)
