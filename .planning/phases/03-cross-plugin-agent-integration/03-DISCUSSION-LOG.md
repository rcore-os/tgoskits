# Phase 3: Cross-Plugin Agent Integration - Discussion Log

**Session:** 2026-05-14
**Mode:** --auto (autonomous)
**Result:** 7 gray areas resolved

## Auto-Selected Decisions

### Sub-Agent Delegation Architecture
**Q:** Should pr-review spawn 3 sub-agents always or conditionally?
**Selected:** Conditional based on PR characteristics
**Reason:** Avoids spawning expensive sub-agents when not needed.

### Confidence Scoring
**Q:** How is the 0-100 confidence score computed?
**Selected:** Explicit assignment by reviewer, secondary axis alongside BLOCK/WARN/INFO
**Reason:** Formula-based scoring unreliable for OS-specific review.

### PR Eligibility Check
**Q:** When does eligibility check run?
**Selected:** Pre-filter before sub-agent spawns
**Reason:** Avoids expensive sub-agent spawns for closed/draft/reviewed PRs.

### Bug-Hunt TDD Integration
**Q:** Does TDD change bug-hunt's workflow structure?
**Selected:** Fits into existing repro→fix→verify flow
**Reason:** TDD's RED/GREEN/REFACTOR maps to bug-hunt's existing phases.

### Agent Skill Loading
**Q:** How are skills loaded across 5 agents?
**Selected:** Frontmatter only (matching Phase 01/02 pattern)
**Reason:** Dependency Check preamble already validates at invocation.

### Sub-Agent Spawn References
**Q:** Where do spawn references appear?
**Selected:** Preamble Agents section + body description (Phase 01 pattern)
**Reason:** Consistent with existing impl→test-gen pattern.

### Regression Safety
**Q:** How to ensure no regression?
**Selected:** Additive only — no structural changes to existing workflows
**Reason:** Skills and spawns enhance without replacing existing checks.
