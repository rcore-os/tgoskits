# Phase 3: Cross-Plugin Agent Integration - Context

**Gathered:** 2026-05-14
**Status:** Ready for planning

## Phase Boundary

Fully compose all 5 remaining TGOSKits agents with installed plugins. pr-review gains sub-agent delegation (code-reviewer, pr-test-analyzer, silent-failure-hunter) for deep PR analysis with confidence scoring. bug-hunt, impl, test-gen, and driver-audit load their assigned plugin skills and spawn their delegated sub-agents. All changes must preserve existing agent workflows without regression.

**In scope:** pr-review (3 sub-agents + confidence scoring + eligibility check + receiving-code-review), bug-hunt (2 skills + 1 sub-agent), impl (1 skill + 2 sub-agents), test-gen (2 skills), driver-audit (1 skill + 1 sub-agent)
**Out of scope:** self-evolve (Phase 2), new agents, agent workflow redesigns, inter-agent orchestration

## Implementation Decisions

### Sub-Agent Delegation Architecture (CPI-01, CPI-04, CPI-05)
- **D-01:** pr-review spawns sub-agents **conditionally based on PR characteristics**, not all at once:
  - `pr-review-toolkit:code-reviewer` — always, as a pre-filter for general code quality before OS-specific review
  - `pr-review-toolkit:pr-test-analyzer` — only when the PR diff shows test changes OR the PR is labeled as non-concurrency
  - `pr-review-toolkit:silent-failure-hunter` — only when the PR diff shows error-handling patterns (try/catch, Result, Option, unwrap, expect)

  ```
  [auto] sub-agent delegation — Selected: conditional based on PR characteristics (recommended)
  ```

### Confidence Scoring (CPI-03)
- **D-02:** Confidence score (0-100) is **explicitly assigned** by pr-review, not computed via formula. It appears as a **secondary axis** alongside BLOCK/WARN/INFO in a dedicated "Confidence Assessment" section after findings. The score reflects the reviewer's confidence that all relevant issues have been identified, factoring in: PR complexity, diff size, test coverage, and sub-agent findings.

  ```
  [auto] confidence scoring — Selected: explicit assignment + secondary axis (recommended)
  ```

### PR Eligibility Check (CPI-02)
- **D-03:** pr-review skips review for closed, draft, and already-reviewed PRs **before** spawning sub-agents. The eligibility check runs first (lightweight: reads PR state from GitHub API or PR metadata), and only eligible PRs proceed to the code-reviewer pre-filter. This avoids spawning expensive sub-agents for PRs that don't need review.

  ```
  [auto] eligibility check — Selected: pre-filter before sub-agent spawns (recommended)
  ```

### Bug-Hunt TDD Integration (CPI-06, CPI-07)
- **D-04:** `superpowers:test-driven-development` fits into bug-hunt's existing "repro → fix → verify" flow without structural changes. The TDD skill guides RED (write failing repro test) / GREEN (apply fix) / REFACTOR (clean up) phases. `superpowers:dispatching-parallel-agents` is invoked when bug-hunt needs to run multiple repro scenarios in parallel. `pr-review-toolkit:silent-failure-hunter` is spawned for bugs classified as error-handling issues.

  ```
  [auto] TDD integration — Selected: fits into existing flow (recommended)
  ```

### Agent Skill Loading Strategy (CPI-06 through CPI-16)
- **D-05:** Skills are loaded via **frontmatter only** (matching Phase 01 and Phase 02 patterns). All assigned skills added to each agent's `skills:` field. The Dependency Check preamble (added in Phase 01) already validates these at invocation time. No inline Skill() invocations needed — agents reference skills naturally in their workflow descriptions.

  ```
  [auto] skill loading — Selected: frontmatter only (recommended)
  ```

### Sub-Agent Spawn References (CPI-12, CPI-13, CPI-14, CPI-15)
- **D-06:** Sub-agent spawn references appear in agent body text (matching Phase 01 pattern for impl→test-gen). Each agent lists its spawn targets in its Dependency Check preamble's "Agents" section plus describes when/how to spawn them in the body. Graceful handling: if sub-agent unavailable, warn and continue with manual analysis (consistent with Phase 01/02 fallback pattern).

  ```
  [auto] spawn references — Selected: preamble Agents section + body description (recommended per Phase 01 pattern)
  ```

### Regression Safety (Cross-cutting)
- **D-07:** All 5 agents preserve their existing workflow structure. Skills and sub-agent spawns are **additive** — they enhance existing dimensions without replacing them. The Phase 01 Dependency Check preambles already validate the new skills — no structural preamble changes needed (only listing the new entries). Test verification: each agent's original workflow triggers remain unchanged.

  ```
  [auto] regression safety — Selected: additive only, no structural changes (recommended)
  ```

### Claude's Discretion
- Exact wording of confidence scoring criteria in pr-review body
- Threshold for "large diff" vs "small diff" in eligibility check
- Whether silent-failure-hunter spawn precedes or follows manual review in bug-hunt
- Exact ordering of sub-agent spawns in pr-review (code-reviewer always first)
- Whether confidence score appears per-finding or per-review

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project Planning
- `.planning/ROADMAP.md` — Phase 3 goal and 5 success criteria
- `.planning/REQUIREMENTS.md` — CPI-01 through CPI-16 with traceability
- `.planning/PROJECT.md` — Project constraints, out-of-scope items

### Phase 1 & 2 Decisions (binding context)
- `.planning/phases/01-foundation-risk-mitigation/01-CONTEXT.md` — D-01 through D-09 (preamble pattern, validation layers, graceful fallback)
- `.planning/phases/02-self-evolve-enhancement/02-CONTEXT.md` — D-01 through D-06 (sub-agent spawn patterns, skill loading, collision detection)

### Agent Files (current state — all have Phase 01 Dependency Check preambles)
- `.claude/agents/pr-review.md` — Phase 01 frontmatter (WebSearch, WebFetch, systematic-debugging). Body: Global Capabilities, Review Dimensions, BLOCK/WARN/INFO classification
- `.claude/agents/bug-hunt.md` — Phase 01 frontmatter (WebSearch, WebFetch, systematic-debugging). Body: Bug Classification, Global Capabilities, repro→fix→verify workflow
- `.claude/agents/impl.md` — Phase 01 frontmatter. Body: feature implementation with test-gen spawn
- `.claude/agents/test-gen.md` — Phase 01 frontmatter. Body: test generation methodology
- `.claude/agents/driver-audit.md` — Phase 01 frontmatter. Body: Driver Core/Capability/OS Glue/Runtime layer audit
- `.claude/agents/self-evolve.md` — Phase 02 enhanced (reference: sub-agent spawn + skill invocation patterns)

### External Dependencies
- `~/.claude/plugins/installed_plugins.json` — Authoritative plugin presence source
- `~/.claude/plugins/cache/claude-plugins-official/pr-review-toolkit/` — code-reviewer, pr-test-analyzer, silent-failure-hunter agents
- `~/.claude/plugins/cache/claude-plugins-official/superpowers/` — test-driven-development, dispatching-parallel-agents, systematic-debugging, brainstorming, receiving-code-review skills

## Existing Code Insights

### Reusable Assets
- **Phase 01 Dependency Check preamble** (all 6 agents): Standardized pattern for listing skills, tools, and agent spawn targets. New content only adds to existing lists.
- **Phase 02 sub-agent spawn pattern** (`self-evolve.md` D2+D3 section): Documents spawning plugin-validator with severity mapping, fallback, and one-spawn-per-cycle. pr-review follows the same pattern for its 3 sub-agents.
- **Phase 02 skill invocation pattern** (`self-evolve.md` D1/D6/D7): Inline references to plugin-dev skills by dimension. All 5 agents follow this for their assigned skills.
- **BLOCK/WARN/INFO classification** (`pr-review.md`): Existing review dimension used by confidence scoring as secondary axis.
- **Bug classification system** (`bug-hunt.md`): Root Cause × Manifestation dimensions. TDD fits naturally into the repro→fix→verify cycle.

### Established Patterns
- Frontmatter skills format: `plugin:skill-name` for external, bare name for project-local
- Sub-agent spawn references in body text, validated in Dependency Check preamble
- Graceful fallback: if plugin/sub-agent unavailable, warn and continue with manual path
- Additive enhancement: existing workflow structure preserved, new content added alongside

### Integration Points
- **pr-review frontmatter:** Add receiving-code-review skill
- **pr-review body:** Add eligibility check (before existing flow), code-reviewer pre-filter (after eligibility, before OS review), conditional pr-test-analyzer and silent-failure-hunter spawns, confidence scoring section (after findings)
- **pr-review preamble:** Add code-reviewer, pr-test-analyzer, silent-failure-hunter to Agents section
- **bug-hunt frontmatter:** Add test-driven-development, dispatching-parallel-agents skills
- **bug-hunt body:** Add TDD phases to repro→fix→verify, add silent-failure-hunter spawn for error-handling bugs
- **bug-hunt preamble:** Add silent-failure-hunter to Agents section
- **impl frontmatter:** Add systematic-debugging skill
- **impl body:** Add type-design-analyzer spawn for new types, code-simplifier spawn for post-implementation
- **impl preamble:** Add type-design-analyzer, code-simplifier to Agents section
- **test-gen frontmatter:** Add brainstorming, test-driven-development skills
- **driver-audit frontmatter:** Add systematic-debugging skill
- **driver-audit body:** Add type-design-analyzer spawn for trait review
- **driver-audit preamble:** Add type-design-analyzer to Agents section

## Specific Ideas

No specific references — open to standard approaches. The researcher should verify that all referenced pr-review-toolkit sub-agents and superpowers skills are actually installed.

## Deferred Ideas

None — discussion stayed within phase scope.

---

*Phase: 03-cross-plugin-agent-integration*
*Context gathered: 2026-05-14*
