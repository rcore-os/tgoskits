# Phase 1: Foundation (Risk Mitigation) - Context

**Gathered:** 2026-05-13
**Status:** Ready for planning

## Phase Boundary

Eliminate silent failure risks in the TGOSKits Claude Code plugin. All 6 custom agents must validate their cross-plugin dependencies at invocation time, failing with clear, actionable error messages when required plugins, skills, or sub-agents are unavailable.

This phase delivers: dependency documentation in CLAUDE.md, a pre-invoke shell hook for fast validation, standardized in-agent preamble checks, and removal of the uninstalled security-auditor references.

## Implementation Decisions

### Dependency Documentation (FND-01)
- **D-01:** Hard dependencies documented as an inline section in project CLAUDE.md with a table format including plugin name, minimum version, what it provides, and copy-paste install commands.
- **D-02:** Required plugins: `superpowers` >= 5.1.0, `pr-review-toolkit`. The `code-modernization` plugin (security-auditor) is explicitly removed as a dependency — no agent references it.

### Validation Mechanism (FND-02, FND-03)
- **D-03:** Two-layer validation: a shell script pre-invoke hook checks `installed_plugins.json` for fast failure before agent context loads, plus a minimal standardized preamble block at the top of each agent's markdown body.
- **D-04:** The shell script hook validates against the live `~/.claude/plugins/installed_plugins.json` file. It checks plugin presence and minimum versions.
- **D-05:** The in-agent preamble is a short standardized block: lists required skills/tools/agents, verifies each resolves, and aborts with clear error if any are missing. Same block structure across all 6 agents.

### Validation Failure Behavior (FND-02, FND-03)
- **D-06:** On validation failure: abort the agent invocation with a clear error message listing exactly what's missing, followed by an offer to auto-fix (batch install command for all missing dependencies at once with a single consent prompt).
- **D-07:** Batch fix approach: all missing dependencies listed together in one prompt rather than individual prompts per dependency.

### Validation Scope (FND-02, FND-03)
- **D-08:** Validation covers all three reference layers: frontmatter `skills:` entries, frontmatter `tools:` entries, AND spawned-agent references in the agent body text.
- **D-09:** All `security-auditor` spawn references removed from pr-review, bug-hunt, impl, and driver-audit agents. Remaining spawn targets for detection: `debugger` (bug-hunt), `test-gen` (impl).

### Claude's Discretion
- Exact wording of error messages in the preamble block
- Specific shell commands used in the validation hook
- Ordering of checks within each validation layer

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project Planning
- `.planning/ROADMAP.md` — Phase definitions, dependencies, and success criteria
- `.planning/REQUIREMENTS.md` — FND-01 through FND-05 with traceability
- `.planning/PROJECT.md` — Project constraints (plugin boundary, compatibility, no regression)

### Plugin Structure
- `.claude/plugin.json` — Plugin manifest with registered agents, commands, hooks
- `.claude/hooks/hooks.json` — Existing hook registrations
- `.claude/settings.json` — Project settings

### Agent Files (current state — will be modified)
- `.claude/agents/pr-review.md` — References security-auditor, missing WebSearch/WebFetch/superpowers:systematic-debugging
- `.claude/agents/bug-hunt.md` — References security-auditor, debugger; missing WebSearch/WebFetch
- `.claude/agents/impl.md` — References security-auditor, test-gen; tools complete
- `.claude/agents/driver-audit.md` — References security-auditor
- `.claude/agents/test-gen.md` — No external spawn references
- `.claude/agents/self-evolve.md` — Plugin self-audit, no external spawn references

### External Dependencies
- `~/.claude/plugins/installed_plugins.json` — Canonical source for installed plugin verification
- `~/.claude/plugins/cache/claude-plugins-official/superpowers/5.1.0/skills/` — Available superpowers skills
- `~/.claude/plugins/cache/claude-plugins-official/pr-review-toolkit/` — Available pr-review-toolkit agents

## Existing Code Insights

### Reusable Assets
- **Existing hooks system** (`.claude/hooks/`): Already has `hooks.json` with pre-PR-gate hook. The new validation hook follows the same pattern.
- **Plugin validation patterns in self-evolve** (`.claude/agents/self-evolve.md` D1-D4): Self-evolve already checks path correctness and cross-references. The new validation reuses similar logic but at invocation time rather than audit time.
- **installed_plugins.json**: The authoritative source for plugin presence checking — stable format, always up-to-date.

### Established Patterns
- Agent frontmatter uses YAML with `skills:`, `tools:`, and `model:` fields. Validation must parse these consistently.
- Skills are referenced as `plugin:skill-name` (e.g., `superpowers:verification-before-completion`). Project-local skills have no prefix.
- Spawned agents are referenced inline in body text as `` `agent-name` ``.

### Integration Points
- **Hook registration:** New hook entry in `.claude/hooks/hooks.json` (alongside existing `pre-pr-gate` hook)
- **Agent frontmatter:** `skills:` and `tools:` fields updated in pr-review.md, bug-hunt.md
- **CLAUDE.md:** New dependency section inserted (location TBD by Claude)

## Specific Ideas

- security-auditor removal is definitive — no fallback, no conditional spawning. If security review capabilities are needed later, that's a separate phase.
- The preamble block should be visually distinct (e.g., `### Dependency Check` heading) so it's easy to find and audit.
- Validation should fail fast on the first missing dependency rather than collecting all failures — simpler, less context burn.

## Deferred Ideas

None — discussion stayed within phase scope.

---

*Phase: 01-Foundation (Risk Mitigation)*
*Context gathered: 2026-05-13*
