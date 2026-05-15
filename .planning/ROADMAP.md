# Roadmap: TGOSKits Plugin Enhancement

## Overview
**3 phases** | **29 requirements** | **Core Value:** TGOSKits agents deliver higher-quality OS development output by composing specialized installed plugins rather than duplicating their logic.

---

### Phase 1: Foundation (Risk Mitigation)
**Goal:** Eliminate silent failure risks; all agents validate their cross-plugin dependencies at startup
**Mode:** mvp
**Requirements:** FND-01 through FND-05
**Plans:** 4 plans (2 main + 2 gap closure)

**Success Criteria:**
1. CLAUDE.md documents superpowers >= 5.1.0 and pr-review-toolkit as hard dependencies with explicit installation instructions
2. All 6 agents fail with clear error messages at invocation when required plugins, skills, or agents are unavailable
3. pr-review and bug-hunt can access man pages and web documentation via WebSearch/WebFetch tools added to their frontmatter
4. pr-review agent loads superpowers:systematic-debugging methodology for root-cause analysis during bug classification
5. Silent cross-plugin skill resolution failures are eliminated -- any missing `plugin:skill` reference produces a loud startup error

**Plans:**
- [x] 01-01-PLAN.md -- SessionStart validation hook + dependency documentation in CLAUDE.md
- [x] 01-02-PLAN.md -- In-agent preamble blocks + agent updates (security-auditor removal, WebSearch/WebFetch, systematic-debugging)
- [x] 01-03-PLAN.md -- [GAP CLOSURE] Fix parse_version("unknown") failing minimum version check; add missing test case
- [x] 01-04-PLAN.md -- [GAP CLOSURE] Document debugger-to-skill routing decision in bug-hunt preamble and SUMMARY

---

### Phase 2: Self-Evolve Enhancement
**Goal:** self-evolve leverages plugin-dev for automated quality auditing instead of manual review
**Mode:** mvp
**Requirements:** SE-01 through SE-08
**Plans:** 3 plans

**Success Criteria:**
1. self-evolve D2 (Syntax) and D3 (Frontmatter) checks run automatically via plugin-dev:plugin-validator sub-agent; zero false positives
2. D4 cross-reference check validates all `plugin:skill` references against the live `installed_plugins.json` file, catching stale or broken references
3. self-evolve loads all 5 plugin-dev skills (agent-development, skill-development, hook-development, plus plugin-validator for automation) for comprehensive plugin quality auditing
4. Optional agent-name collision detection scans globally installed plugins and warns on namespace conflicts (e.g., `impl`, `test`, `pr-review`)
5. Improvement ideation phase uses superpowers:brainstorming; root-cause analysis of quality issues uses superpowers:systematic-debugging

**Plans:**
- [x] 02-01-PLAN.md -- Frontmatter skills (7 new) + Dependency Check preamble with fallback documentation
- [x] 02-02-PLAN.md -- D2/D3 sub-agent spawn + D4 installed_plugins.json cross-reference + collision detection
- [x] 02-03-PLAN.md -- D1/D6/D7 skill invocations + brainstorming phase + systematic-debugging cross-cut

---

### Phase 3: Cross-Plugin Agent Integration
**Goal:** All 6 agents fully compose installed plugins -- skills for methodology, agent delegation for deliverables
**Mode:** mvp
**Requirements:** CPI-01 through CPI-16
**Plans:** 5 plans

**Success Criteria:**
1. pr-review pre-filters PRs through pr-review-toolkit:code-reviewer for general code quality before performing OS-specific review (POSIX/Linux semantics)
2. Confidence scoring (0-100) appears as a secondary axis alongside BLOCK/WARN/INFO dimensions in all pr-review output
3. pr-review spawns pr-test-analyzer for non-concurrency PRs and silent-failure-hunter for error-handling-focused PRs -- each targeted to the appropriate review context
4. bug-hunt loads test-driven-development for repro-test-before-fix workflow and spawns silent-failure-hunter for error-handling bug investigations
5. All 6 agents load their assigned installed-plugin skills and spawn their delegated sub-agents without regression in existing functionality

**Plans:**
- [x] 03-01-PLAN.md — test-gen: add brainstorming + TDD skills to frontmatter and body (CPI-09, CPI-10)
- [x] 03-02-PLAN.md — driver-audit: add systematic-debugging skill + type-design-analyzer spawn (CPI-11, CPI-15)
- [x] 03-03-PLAN.md — bug-hunt: add TDD + parallel-agents skills + silent-failure-hunter spawn (CPI-06, CPI-07, CPI-12)
- [x] 03-04-PLAN.md — impl: add systematic-debugging skill + type-design-analyzer + code-simplifier spawns (CPI-08, CPI-13, CPI-14)
- [x] 03-05-PLAN.md — pr-review: add eligibility check + code-reviewer pre-filter + conditional spawns + confidence scoring (CPI-01..05, CPI-16)

---

## Phase Dependencies

```
Phase 1 (Foundation)
  └── Phase 2 (Self-Evolve Enhancement)
       └── Phase 3 (Cross-Plugin Agent Integration)
```

Phase 1 is a strict prerequisite: silent failure risks must be eliminated before adding new cross-plugin integrations. Phase 2 enhances self-evolve with plugin-dev tooling so it can validate the Phase 3 integrations. Phase 3 adds the remaining skill references and agent delegation across all 6 agents.

## Requirement Coverage

| Category | Requirements | Phase |
|----------|-------------|-------|
| Foundation (FND) | 5 (FND-01 through FND-05) | Phase 1 |
| Self-Evolve (SE) | 8 (SE-01 through SE-08) | Phase 2 |
| Cross-Plugin Integration (CPI) | 16 (CPI-01 through CPI-16) | Phase 3 |
| **Total** | **29** | **3 phases** |

Coverage: 29/29 requirements mapped (100%). See REQUIREMENTS.md Traceability for per-requirement mapping.

---
*Roadmap created: 2026-05-13*
*Last updated: 2026-05-14 (Phase 3 plans added)*
