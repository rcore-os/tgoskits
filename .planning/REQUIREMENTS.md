# Requirements: TGOSKits Plugin Enhancement

**Defined:** 2026-05-13
**Core Value:** TGOSKits agents deliver higher-quality OS development output by composing specialized installed plugins rather than duplicating their logic.

## v1 Requirements

### Foundation (Risk Mitigation)

- [ ] **FND-01**: Document required global plugins (superpowers >= 5.1.0) and agents (security-auditor) as hard dependencies in project CLAUDE.md
- [ ] **FND-02**: Add startup validation to all 6 agents that checks referenced `plugin:skill` entries resolve against installed plugins, failing loudly if not found
- [ ] **FND-03**: Add missing-agent detection to pr-review (spawns security-auditor), bug-hunt (spawns security-auditor), impl (spawns test-gen, security-auditor), driver-audit (spawns security-auditor) — fail with clear error if spawn target unavailable
- [ ] **FND-04**: Add `WebSearch` + `WebFetch` tools to pr-review and bug-hunt agent frontmatter
- [ ] **FND-05**: Add `superpowers:systematic-debugging` skill to pr-review agent frontmatter

### Self-Evolve Enhancement

- [ ] **SE-01**: self-evolve agent spawns `plugin-dev:plugin-validator` to automate D2 (Syntax) + D3 (Frontmatter) checks
- [ ] **SE-02**: self-evolve agent extends D4 cross-reference check to validate all `plugin:skill` references against `installed_plugins.json`
- [ ] **SE-03**: self-evolve agent loads `plugin-dev:skill-development` skill for D3 skill quality review
- [ ] **SE-04**: self-evolve agent loads `plugin-dev:agent-development` skill for D4 agent quality review
- [ ] **SE-05**: self-evolve agent loads `plugin-dev:hook-development` skill for D7 hook validation
- [ ] **SE-06**: self-evolve agent adds optional agent-name collision detection against globally installed plugins
- [ ] **SE-07**: self-evolve agent loads `superpowers:brainstorming` skill for improvement ideation phase
- [ ] **SE-08**: self-evolve agent loads `superpowers:systematic-debugging` skill for root-cause analysis of quality issues

### Cross-Plugin Agent Integration

- [ ] **CPI-01**: pr-review agent spawns `pr-review-toolkit:code-reviewer` as pre-filter for general code quality before OS-specific review
- [ ] **CPI-02**: pr-review agent adopts code-review eligibility check pattern (skip closed/draft/reviewed PRs before deep analysis)
- [ ] **CPI-03**: pr-review agent adds confidence scoring (0-100) as secondary axis alongside existing BLOCK/WARN/INFO dimensions
- [ ] **CPI-04**: pr-review agent spawns `pr-review-toolkit:pr-test-analyzer` for non-concurrency PRs to audit test coverage
- [ ] **CPI-05**: pr-review agent spawns `pr-review-toolkit:silent-failure-hunter` for error-handling-focused PRs
- [x] **CPI-06**: bug-hunt agent loads `superpowers:test-driven-development` skill for repro-test-before-fix workflow
- [x] **CPI-07**: bug-hunt agent loads `superpowers:dispatching-parallel-agents` skill for parallel test execution
- [x] **CPI-08**: impl agent loads `superpowers:systematic-debugging` skill for implementation debugging
- [ ] **CPI-09**: test-gen agent loads `superpowers:brainstorming` skill for test design ideation
- [ ] **CPI-10**: test-gen agent loads `superpowers:test-driven-development` skill for TDD methodology
- [ ] **CPI-11**: driver-audit agent loads `superpowers:systematic-debugging` skill for layering violation analysis
- [x] **CPI-12**: bug-hunt agent spawns `pr-review-toolkit:silent-failure-hunter` for error-handling bug investigations
- [x] **CPI-13**: impl agent spawns `pr-review-toolkit:type-design-analyzer` when introducing new types or traits
- [x] **CPI-14**: impl agent spawns `pr-review-toolkit:code-simplifier` for post-implementation code polish
- [ ] **CPI-15**: driver-audit agent spawns `pr-review-toolkit:type-design-analyzer` for trait design review
- [ ] **CPI-16**: pr-review agent loads `superpowers:receiving-code-review` skill for handling review feedback

## v2 Requirements

Deferred to future release.

- **ADV-01**: Add version snapshot mechanism to detect installed plugin drift (SHA256 of plugin files)
- **ADV-02**: Add `plugin.json` dependency declaration field for cross-plugin dependencies
- **ADV-03**: Consider namespace prefix (`tg-`) for generic agent names to prevent collisions
- **ADV-04**: Build agent test harness that validates agent behavior under different CLAUDE.md configurations

## Out of Scope

| Feature | Reason |
|---------|--------|
| Creating new agents beyond existing 6 | Focus is enhancing what exists, not adding new agent types |
| Modifying commands or hooks | Scope is agent enhancement only |
| Forking/modifying installed plugins | Consume only — don't maintain forks of superpowers, pr-review-toolkit, etc. |
| Workflow orchestration (agent pipelines) | Complex chaining of plugins is a separate project |
| plugin-dev:create-plugin command integration | TGOSKits plugin already exists; no need to scaffold new plugins |
| code-review:code-review slash command integration | Redundant with pr-review agent + pr-review-toolkit:code-reviewer |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| FND-01 | Phase 1 | Pending |
| FND-02 | Phase 1 | Pending |
| FND-03 | Phase 1 | Pending |
| FND-04 | Phase 1 | Pending |
| FND-05 | Phase 1 | Pending |
| SE-01 | Phase 2 | Pending |
| SE-02 | Phase 2 | Pending |
| SE-03 | Phase 2 | Pending |
| SE-04 | Phase 2 | Pending |
| SE-05 | Phase 2 | Pending |
| SE-06 | Phase 2 | Pending |
| SE-07 | Phase 2 | Pending |
| SE-08 | Phase 2 | Pending |
| CPI-01 | Phase 3 | Pending |
| CPI-02 | Phase 3 | Pending |
| CPI-03 | Phase 3 | Pending |
| CPI-04 | Phase 3 | Pending |
| CPI-05 | Phase 3 | Pending |
| CPI-06 | Phase 3 | Completed |
| CPI-07 | Phase 3 | Completed |
| CPI-08 | Phase 3 | Completed |
| CPI-09 | Phase 3 | Pending |
| CPI-10 | Phase 3 | Pending |
| CPI-11 | Phase 3 | Pending |
| CPI-12 | Phase 3 | Completed |
| CPI-13 | Phase 3 | Completed |
| CPI-14 | Phase 3 | Completed |
| CPI-15 | Phase 3 | Pending |
| CPI-16 | Phase 3 | Pending |

**Coverage:**
- v1 requirements: 29 total
- Mapped to phases: 29
- Unmapped: 0

---
*Requirements defined: 2026-05-13*
*Last updated: 2026-05-14 (CPI-08, CPI-13, CPI-14 completed)*
