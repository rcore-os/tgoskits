# TGOSKits Agent Architecture

**Status**: Research output
**Date**: 2026-05-13
**Dimension**: ARCHITECTURE

---

## 1. Agent Landscape

### 1.1 The Six TGOSKits Agents

| Agent | Role | Tier | Spawns Sub-Agents? | Auto-Fixes? |
|-------|------|------|--------------------|-------------|
| `pr-review` | Kernel code reviewer — POSIX/Linux correctness, safety, concurrency | Orchestrator | `security-auditor` | Yes (BLOCK items) |
| `bug-hunt` | Bug discovery → repro → fix → verify → PR | Orchestrator | `debugger`, `security-auditor`, `pr-review` | Yes (own bugs) |
| `impl` | Feature implementation: discover → plan → test → implement → CI → PR | Orchestrator | `test-gen`, `pr-review`, `security-auditor` | Yes (own code) |
| `test-gen` | Generate test cases from Linux reference behavior | Specialist | None | No (read-only creator) |
| `driver-audit` | Audit driver layer separation (Core/Capability/OS Glue/Runtime) | Specialist | `security-auditor` | No (hardware testing needed) |
| `self-evolve` | Self-audit .claude/ plugin files, find issues, fix, validate | Meta | None | Yes |

### 1.2 Installed Plugins Providing Agents

| Plugin | Agents | Used By |
|--------|--------|---------|
| `pr-review-toolkit` | `code-reviewer`, `code-simplifier`, `type-design-analyzer`, `silent-failure-hunter`, `pr-test-analyzer`, `comment-analyzer` | None directly (potential integration point) |
| User `.agent/` | `security-auditor`, `debugger`, others | `pr-review`, `bug-hunt`, `impl`, `driver-audit` |

### 1.3 Installed Plugins Providing Skills

| Plugin | Skill | Guidance Model | Used By |
|--------|-------|----------------|---------|
| `superpowers` | `verification-before-completion` | Process gate — must read and apply before claiming completion | All 6 agents |
| `superpowers` | `systematic-debugging` | Process methodology — 4-phase debugging protocol | `bug-hunt` |

### 1.4 Project-Local Skills

| Skill | Type | Guidance Model |
|-------|------|----------------|
| `review-open-prs` | Domain knowledge + workflow | Read for PR review methodology, worktree setup, GitHub review submission |
| `starry-test-suit` | Domain knowledge | Read for test layout rules, asset pipeline conventions |
| `arceos-test-adapter` | Domain knowledge | Read for qemu-*.toml config patterns, regex rules |
| `cross-kernel-driver` | Domain knowledge | Read for driver layer rules, MMIO/DMA API, interface shape |
| `update-std-tests` | Utility workflow | Read for CSV audit/update process |
| `board-uboot-fsck-repair` | Recovery workflow | Read for board fsck recovery procedure |

---

## 2. Delegation Architecture

### 2.1 The Two Patterns

**Pattern A: Read Skill for Guidance (Internalize)**
The agent loads a skill's knowledge and applies it within its own execution context. Used when the skill provides process, methodology, domain knowledge, or checklists that the agent must incorporate into its own decision-making.

**Pattern B: Spawn Agent for Work (Delegate)**
The agent spawns a separate agent for a bounded, separable sub-task. Used when the work requires different domain expertise, benefits from independent execution context, or has a clear input/output contract.

### 2.2 Decision Heuristic

```
Is the task...
├── A process/methodology/checklist to follow?     → Pattern A (Read Skill)
├── A bounded sub-task with clear I/O contract?    → Pattern B (Spawn Agent)
├── OS/kernel domain knowledge the agent needs?    → Pattern A (Read Skill)
├── A different domain (security, debugging)?      → Pattern B (Spawn Agent)
└── A pure verification gate before proceeding?    → Pattern A (Read Skill)
```

### 2.3 Per-Agent Delegation Map

#### pr-review

```
pr-review
├── READS skill: review-open-prs          [GitHub review workflow, worktree, submission]
├── READS skill: starry-test-suit         [test layout/validation conventions]
├── READS skill: arceos-test-adapter      [config patterns, regex rules]
├── READS skill: verification-before-completion  [gate before claiming complete]
├── SPAWNS agent: security-auditor        [when diff has unsafe blocks, raw pointers, MMIO/DMA]
└── RUNS command: local-ci.sh             [bash, not agent spawn]
```

**Why spawn security-auditor?** Security review is a bounded task with different expertise. The pr-review agent knows *when* to ask for security review but the security-auditor owns the *how*.

**Why read review-open-prs?** The skill provides methodology (worktree setup, review thread management, validation commands) that the pr-review agent MUST internalize. It cannot delegate "setting up worktrees" or "deciding review outcome."

#### bug-hunt

```
bug-hunt
├── READS skill: starry-test-suit         [test layout conventions for repro tests]
├── READS skill: cross-kernel-driver      [driver layer rules if bug is in drivers/]
├── READS skill: arceos-test-adapter      [config patterns for ArceOS tests]
├── READS skill: systematic-debugging     [4-phase debugging methodology]
├── READS skill: verification-before-completion  [gate before claiming fix complete]
├── SPAWNS agent: debugger                [complex crashes, memory corruption, multi-core races]
├── SPAWNS agent: security-auditor        [memory-bug, validation-bug, access-bug fixes]
├── SPAWNS agent: pr-review               [Phase 5: self-review before PR creation]
└── RUNS command: local-ci.sh, syscall-diff.py, journal-generator.py  [bash]
```

**Why read systematic-debugging?** The skill defines HOW to approach debugging — reproducing, root cause tracing, hypothesis testing. This is methodology, not a separable task.

**Why spawn debugger?** For complex cases (kernel panics, memory corruption), the debugger agent provides specialized debugging expertise as a bounded analysis task.

**Why spawn pr-review?** Phase 5 of bug-hunt explicitly says "Launch the PR-Review Agent." This is a clean delegation: pr-review gets the diff, returns a REVIEW.md with BLOCK/WARN/INFO.

#### impl

```
impl
├── READS skill: starry-test-suit         [test layout for generated tests]
├── READS skill: arceos-test-adapter      [ArceOS test conventions]
├── READS skill: verification-before-completion  [gate at each phase boundary]
├── SPAWNS agent: test-gen                [Phase 3: generate test cases from feature list]
├── SPAWNS agent: security-auditor        [Phase 4/6: memory safety review]
├── SPAWNS agent: pr-review               [Phase 6a: pre-PR code review]
├── USES MCP: context7                    [syscall semantics documentation]
└── USES TOOL: WebSearch, WebFetch        [man-pages, POSIX specs]
```

**Why spawn test-gen?** Phase 3 is a perfect delegation boundary: input = feature list, output = test directory with C programs, CMakeLists, and qemu-*.toml files. The impl agent does not need to know test boilerplate layout.

**Why spawn pr-review?** Same as bug-hunt — clean delegation with REVIEW.md as contract.

#### test-gen

```
test-gen
├── READS skill: starry-test-suit         [test layout conventions]
├── READS skill: arceos-test-adapter      [ArceOS test conventions]
├── READS skill: verification-before-completion  [gate: tests pass on Linux AND target OS]
├── SPAWNS agent: (none)
└── RUNS command: docker (strace capture), cargo xtask (QEMU validation)
```

**Pure specialist.** No sub-agent spawning. Its only external interaction is validating tests against Linux (reference) and target OS.

#### driver-audit

```
driver-audit
├── READS skill: cross-kernel-driver      [the primary domain knowledge — all 4 layers]
├── READS skill: verification-before-completion  [gate before finalizing AUDIT.md]
├── SPAWNS agent: security-auditor        [DMA, MMIO register access vulnerability review]
└── USES MCP: context7                    [mmio-api and dma-api crate docs]
```

**Why read cross-kernel-driver?** This is THE domain skill for driver-audit. All audit checks (OS import search, raw pointer MMIO detection, hardcoded IRQ detection) derive from the skill's rules.

**Why spawn security-auditor?** DMA/MMIO security is a separate concern. The driver-audit agent knows the layering rules; the security-auditor knows vulnerability patterns.

**Why NO Write/Bash tools?** driver-audit is read-only by design. Driver changes require hardware testing — the agent reports findings but does not auto-fix.

#### self-evolve

```
self-evolve
├── READS skill: verification-before-completion  [gate after each round's validation]
├── SPAWNS agent: (none)
└── RUNS command: python3 JSON/compile checks, bash -n syntax checks
```

**Self-contained meta-agent.** Operates only on `.claude/` files. Loads no project-local skills because its domain is plugin correctness, not OS/kernel logic.

### 2.4 Agent-to-Agent Communication Contract

Every spawn uses this contract:

```
Caller provides:
  - Context: "current branch", "diff scope", "target syscall list"
  - Expected output format: REVIEW.md, AUDIT.md, test directory, etc.
  - Success criteria: what "done" means for this sub-task

Callee returns:
  - Structured output (file on disk or report in conversation)
  - BLOCK/WARN/INFO classification (where applicable)
  - Evidence: commands run, output captured, exit codes

Caller validates:
  - Verify callee output before acting on it (verification-before-completion)
  - Cross-check callee findings against caller's own domain knowledge
```

**Key principle**: The caller NEVER trusts the callee's success report without independent verification. This is enforced by `superpowers:verification-before-completion` in every agent.

---

## 3. Skill Loading Order and Priority

### 3.1 Loading Phases

```
Phase 1: Agent body (self-description, workflow, rules)
           ↓
Phase 2: Project-local skills (domain knowledge)
           ↓
Phase 3: Installed skills (process methodology)
           ↓
Phase 4: Spawned agents (bounded sub-tasks)
```

### 3.2 Priority Rules

| Priority | Source | Reason |
|----------|--------|--------|
| P0 | Agent's own body | Defines WHAT the agent does. Non-negotiable. |
| P1 | Project-local skills | Define HOW in THIS codebase. Domain-specific conventions supersede general guidance. |
| P2 | `verification-before-completion` | Gate function — blocks premature completion claims regardless of other rules. |
| P3 | Other installed skills | Provide methodology (debugging, planning). Support P0/P1 but don't override them. |
| P4 | Spawned agent output | Actionable findings to incorporate. Validate before applying. |

### 3.3 Ordering in Frontmatter `skills:`

The `skills:` list should follow semantic grouping, not alphabetical:

```yaml
skills:
  # Group 1: Domain knowledge (project-local)
  - review-open-prs          # What domain this agent operates in
  - starry-test-suit         # Supporting domain knowledge
  - arceos-test-adapter      # Supporting domain knowledge
  - cross-kernel-driver      # Supporting domain knowledge

  # Group 2: Process methodology (installed plugins)
  - superpowers:systematic-debugging     # How to approach problems
  - superpowers:verification-before-completion   # Always last — gate function
```

**Rule**: `verification-before-completion` is always LAST in the skills list. It is the gate that fires before any completion claim — placing it last reflects its role as the final check.

### 3.4 When to Reference Project-Local vs Installed Skills

| Skill Type | When Loaded | How Referenced |
|------------|-------------|----------------|
| Project-local (e.g., `starry-test-suit`) | Always — they define THIS project's conventions | Load into agent's own context; apply rules directly |
| Installed process (e.g., `verification-before-completion`) | Always — they define universal process gates | Load into agent's own context; follow process steps |
| Installed agent-with-skill (e.g., pr-review-toolkit code-reviewer) | Only when that analysis dimension is needed | Spawn as sub-agent; do NOT read the skill inline |
| Installed domain (e.g., `cross-kernel-driver` is project-local) | When operating in that domain | Read skill into context; apply rules directly |

**The critical distinction**: A skill that provides a specialized agent (like `pr-review-toolkit`) is designed for the SPAWN pattern. Reading `code-reviewer.md` inline would duplicate the agent's reasoning — spawn it instead.

---

## 4. Agent Body Structure

### 4.1 Recommended Template

Every TGOSKits agent body should follow this structure:

```markdown
---
name: <agent-name>
description: <concise one-liner>
skills:
  - <project-local-domain-skill-1>
  - <project-local-domain-skill-2>
  - superpowers:verification-before-completion
tools:
  - Read
  - Write        # omit if read-only agent
  - Edit         # omit if read-only agent
  - Bash
  - Grep
  - Glob
  - WebSearch    # add if agent needs man-page/POSIX lookup
  - WebFetch     # add if agent needs man-page/POSIX lookup
---

# <Agent-Name> Agent

<One-paragraph identity statement>

## Global Capabilities                    <-- DELEGATION POINTS

<For each external capability:>
- READ skill "<skill-name>" for <purpose>
- SPAWN agent "<agent-name>" for <bounded-task>
- USE MCP "<mcp-server>" for <purpose>
- USE TOOL "<tool>" for <purpose>

## <Domain-Specific Taxonomy / Classification>

<If the agent classifies things (bugs, review findings, driver violations),
define the taxonomy HERE, before any workflow steps.>

## Workflow

### Phase 1: <Name>

<Step-by-step with exact commands>

### Phase 2: <Name>

...

## Loop Control                           <-- ITERATION GUARDS

| Phase | Max Iterations | Exit Condition |
|-------|---------------|----------------|
| ...   | ...           | ...            |

## <Output Format>

<If the agent produces a structured output (REVIEW.md, AUDIT.md, etc.),
define the exact format HERE.>

## Rules

- <Agent-specific invariant rules>
```

### 4.2 Delegation Points in Agent Body

Delegation should be explicit and at well-defined boundaries:

1. **Global Capabilities section** — declare ALL external capabilities the agent uses
2. **Phase transitions** — delegate between phases (e.g., impl Phase 3 delegates to test-gen)
3. **Conditional spawns** — "if X condition holds, spawn Y agent"
4. **Pre-completion gate** — always invoke `verification-before-completion` before any completion claim

### 4.3 Current Agents: Structural Analysis

| Agent | Has Global Capabilities? | Has Taxonomy? | Has Loop Control? | Has Output Format? | Has Rules? |
|-------|--------------------------|---------------|-------------------|--------------------|------------|
| `pr-review` | Yes (well-structured) | Yes (bug taxonomy table) | Yes (3 iterations) | Yes (REVIEW.md template) | Yes (Safety Checklist) |
| `bug-hunt` | Yes | Yes (bug classification) | Yes (5 CI, 3 review) | Yes (PR body template) | Yes |
| `impl` | Yes | No taxonomy needed | Yes (per-phase table) | Yes (PR template, plan template) | No separate Rules section |
| `test-gen` | Yes (minimal) | No taxonomy needed | No explicit loop | No explicit template | No |
| `driver-audit` | Yes | No taxonomy needed | No loop (read-only) | Yes (AUDIT.md template) | No separate Rules section |
| `self-evolve` | No | Yes (7 audit dimensions) | Yes (5 rounds default) | Yes (final report template) | Yes |

---

## 5. Tool Access Analysis

### 5.1 Current Assignment

| Tool | pr-review | bug-hunt | impl | test-gen | driver-audit | self-evolve |
|------|-----------|----------|------|----------|--------------|-------------|
| Read | x | x | x | x | x | x |
| Write | x | x | x | x | - | x |
| Edit | x | x | x | - | - | x |
| Bash | x | x | x | x | - | x |
| Grep | x | x | x | x | x | x |
| Glob | x | x | x | x | x | x |
| WebSearch | - | - | x | - | - | - |
| WebFetch | - | - | x | - | - | - |

### 5.2 Recommendations

**Missing Write/Bash from driver-audit**: CORRECT. Driver-audit is read-only by design — it audits and reports, but does not modify code. This is an intentional safety boundary.

**Missing Edit from test-gen**: CORRECT but borderline. Test-gen creates new files (Write) but the current assignment omits Edit. If test-gen ever needs to update existing qemu-*.toml files, it would need Edit. Currently safe since it only creates new test directories.

**Missing WebSearch/WebFetch from pr-review and bug-hunt**: GAP. Both agents reference "use web search to look up Linux man-pages" but neither has the tools. Consider adding `WebSearch` and `WebFetch` to both. They can currently use `context7` MCP, but man-page lookup may require broader web access.

**Missing Bash from driver-audit**: INTENTIONAL. The agent runs grep commands but those are executed via the Grep tool, not Bash. The Bash tool is not needed because driver-audit does not run builds, tests, or docker commands.

### 5.3 Tool Access Principles

| Principle | Rationale |
|-----------|-----------|
| Read-only agents omit Write + Edit | Prevents accidental modification (safety boundary) |
| Agents that fix code need Write + Edit | Cannot fix without writing |
| All agents need Read + Grep + Glob | Minimum for codebase exploration |
| Agents that run CI/tests need Bash | Cannot validate without execution |
| Agents that look up specifications need WebSearch + WebFetch | man-pages, POSIX docs, RFCs |
| Agents that create files but don't modify existing ones can omit Edit | test-gen pattern |

---

## 6. Integration Architecture: pr-review-toolkit

### 6.1 Current State

The TGOSKits `pr-review` agent currently does NOT use any pr-review-toolkit agents. It implements its own review dimensions (bug taxonomy, safety checklist, synchronization boundary audit) that are OS/kernel-specific.

### 6.2 Integration Opportunities

The pr-review-toolkit provides 6 specialized agents. Here is where each could integrate:

| pr-review-toolkit Agent | Integration Opportunity | TGOSKits Agent | Priority |
|-------------------------|------------------------|----------------|----------|
| `code-reviewer` | General code quality, CLAUDE.md compliance | `pr-review` (post-kernel-review pass) | Medium |
| `code-simplifier` | Simplify complex code after fixes applied | `bug-hunt`, `impl` (post-fix polish) | Low |
| `silent-failure-hunter` | Error handling audit (catch blocks, fallbacks) | `pr-review` (as a sub-dimension) | Medium |
| `type-design-analyzer` | Type invariant analysis for new types | `impl` (when introducing new types) | Low |
| `pr-test-analyzer` | Test coverage quality assessment | `pr-review` (complement to Test Validity Audit) | Medium |
| `comment-analyzer` | Comment accuracy vs code | `pr-review` (minor dimension) | Low |

### 6.3 Recommended Integration Pattern

The `pr-review` agent should spawn pr-review-toolkit agents as a **second pass** — after OS-specific review is complete:

```
pr-review workflow:
  Phase 1: OS-specific review (what it does now)
    - syscall semantics, safety, concurrency, driver layering
    - Uses domain skills: review-open-prs, starry-test-suit, etc.
  
  Phase 2: Spawn pr-review-toolkit agents (NEW)
    - Spawn code-reviewer on the same diff
    - Spawn silent-failure-hunter on error-handling changes
    - Spawn pr-test-analyzer if test files changed
    
  Phase 3: Merge findings
    - Collate BLOCK/WARN/INFO from all sources
    - OS-specific findings take priority over generic findings
    - Deduplicate overlapping issues
```

This respects the separation: OS domain knowledge stays in TGOSKits agents; general code quality stays in pr-review-toolkit agents.

### 6.4 Anti-Pattern To Avoid

Do NOT read `code-reviewer.md` skill inline into the `pr-review` agent body. The pr-review-toolkit agents are designed to run as autonomous sub-agents with their own execution context. Reading them inline would:
- Bloat the pr-review agent prompt
- Lose the benefit of independent analysis (different "persona")
- Violate the delegation principle

---

## 7. Architecture Summary

### 7.1 Delegation Flow Diagram

```
                    ┌──────────────────────────────────────┐
                    │        self-evolve (meta)             │
                    │  audits .claude/ plugin files         │
                    │  spawns: none                         │
                    └──────────────────────────────────────┘
                                      │
                    ┌─────────────────┴──────────────────┐
                    │                                    │
    ┌───────────────┴──────────────┐    ┌───────────────┴──────────────┐
    │     bug-hunt (orchestrator)  │    │      impl (orchestrator)     │
    │  discover → repro → fix → PR │    │  discover → plan → impl → PR │
    │                              │    │                              │
    │  spawns:                     │    │  spawns:                     │
    │   ├── debugger               │    │   ├── test-gen               │
    │   ├── security-auditor       │    │   ├── pr-review              │
    │   └── pr-review              │    │   └── security-auditor       │
    └──────────────┬───────────────┘    └──────────────┬───────────────┘
                   │                                   │
    ┌──────────────┴───────────────┐                   │
    │   pr-review (orchestrator)   │ ◄─────────────────┘
    │  kernel code review          │
    │                              │
    │  spawns:                     │
    │   └── security-auditor       │
    └──────────────┬───────────────┘
                   │
    ┌──────────────┴───────────────┐
    │   test-gen (specialist)      │
    │  generate test cases         │
    │  spawns: none                │
    └──────────────────────────────┘

    ┌──────────────────────────────┐
    │  driver-audit (specialist)   │
    │  audit driver layering       │
    │  spawns: security-auditor    │
    └──────────────────────────────┘
```

### 7.2 Communication Patterns

| Pattern | Direction | Examples |
|---------|-----------|----------|
| **Delegation (spawn)** | Orchestrator → Specialist | bug-hunt → pr-review, impl → test-gen |
| **Advisory (spawn)** | Any agent → security-auditor | pr-review, bug-hunt, impl, driver-audit |
| **Methodology (read skill)** | All agents | verification-before-completion, systematic-debugging |
| **Domain knowledge (read skill)** | All agents | starry-test-suit, cross-kernel-driver, etc. |

### 7.3 Key Principles

1. **Skills are read; agents are spawned.** Never read an agent's body as a skill substitute. If another plugin provides an agent for a task, spawn it.

2. **Domain knowledge stays in TGOSKits agents.** The OS-specific review dimensions (syscall semantics, safety checklist, synchronization boundary audit) belong in `pr-review` and `bug-hunt`, NOT in generic review agents.

3. **verification-before-completion is non-negotiable.** Every agent loads it as the last skill. It fires before ANY completion claim. This creates a universal quality gate.

4. **Tools match capability.** Read-only agents omit Write/Edit. Agents that look up external specs add WebSearch/WebFetch. Agents that run validation need Bash.

5. **Loop control is explicit.** Every agent with an iterative workflow declares max iterations and exit conditions. This prevents infinite fix-validate cycles.

6. **Output formats are defined in the agent body.** REVIEW.md, AUDIT.md, PR body templates are all in the agent body. Spawned agents return findings; the spawning agent formats them.
