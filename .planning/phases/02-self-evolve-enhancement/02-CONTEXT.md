# Phase 2: Self-Evolve Enhancement - Context

**Gathered:** 2026-05-14
**Status:** Ready for planning

## Phase Boundary

Enhance the self-evolve agent to leverage installed plugin-dev plugin for automated quality auditing instead of manual review. self-evolve currently performs 7 audit dimensions (D1-D7) manually — this phase replaces manual checks in D2 (Syntax) and D3 (Frontmatter) with automated sub-agent validation via plugin-dev:plugin-validator, extends D4 cross-reference to validate plugin:skill references against installed_plugins.json, loads relevant plugin-dev skills for comprehensive auditing, and optionally detects agent-name collisions with globally installed plugins.

**In scope:** self-evolve agent frontmatter changes, D2/D3 automation via sub-agent, D4 extension, skill loading, collision detection.
**Out of scope:** Changes to other 5 agents, modifications to plugin-dev itself, creating new agents, self-evolve workflow changes beyond dependency loading.

## Implementation Decisions

### plugin-validator Invocation Pattern (SE-01)
- **D-01:** self-evolve spawns `plugin-dev:plugin-validator` as a **sub-agent** (via Agent tool) for D2 (Syntax) and D3 (Frontmatter) checks. The plugin-validator agent is designed for this purpose and gets its own context window for comprehensive validation without consuming self-evolve's context.

  ```
  [auto] plugin-validator invocation — Selected: sub-agent spawn (recommended default)
  Reason: plugin-validator is an agent, not a skill; spawning it as sub-agent gives full context window for validation without consuming self-evolve's context budget.
  ```

### D4 Cross-Reference Scope (SE-02)
- **D-02:** D4 validates **all plugin:skill references across all agent files** (`.claude/agents/*.md`), not just self-evolve's own references. This catches stale or broken cross-plugin references globally, aligning with the Phase 01 dependency validation approach. The check reads `~/.claude/plugins/installed_plugins.json` as the authoritative source.

  ```
  [auto] D4 cross-reference scope — Selected: all agent files (recommended default)
  Reason: Aligns with Phase 01's global dependency validation; catches stale references regardless of which agent they're in.
  ```

### Skill Loading Strategy (SE-03, SE-04, SE-05)
- **D-03:** plugin-dev skills are loaded **selectively by audit dimension** rather than all at once. Each audit dimension loads only the skills relevant to its checks, keeping context lean:

  | Dimension | Skills Loaded | Purpose |
  |-----------|--------------|---------|
  | D1 (Paths) | `plugin-dev:plugin-structure` | Verify file/directory layout |
  | D2 (Syntax) | (spawns plugin-validator agent) | Automated syntax validation |
  | D3 (Frontmatter) | `plugin-dev:skill-development` + spawns plugin-validator | Frontmatter validation + skill quality review |
  | D4 (Cross-ref) | `plugin-dev:agent-development` | Agent quality review + cross-reference validation |
  | D5 (Anti-patterns) | (generic — no plugin-dev skill needed) | Hardcoded pattern matching |
  | D6 (Integration) | `plugin-dev:plugin-settings` | Settings/config consistency |
  | D7 (Hooks) | `plugin-dev:hook-development` | Hook structure validation |

  The exact mapping of skills to dimensions is decided at planning time based on researcher findings about each skill's actual capabilities.

  ```
  [auto] skill loading strategy — Selected: selective by audit dimension (recommended default)
  Reason: Keeps context lean and predictable; each dimension has clearly assigned skill(s).
  ```

### Agent-Name Collision Detection (SE-06)
- **D-04:** Collision detection is **simple grep-based**: self-evolve scans globally installed plugins' agent names (from `installed_plugins.json` or plugin manifests) and warns on exact-name matches with TGOSKits agents. Specifically check the risky generic names: `impl`, `test-gen`, `pr-review`. Result is a **warning** (non-blocking) surfaced in the D4 report, not a hard error. The feature flag remains "optional" — it's a quality-of-life improvement, not a gate.

  ```
  [auto] collision detection approach — Selected: simple grep + warning (recommended default)
  Reason: Low-cost to implement, high-value for catching namespace conflicts; warning suffices since actual collisions require user action.
  ```

### plugin-dev Unavailability Handling (SE-01 through SE-08)
- **D-05:** If plugin-dev is not installed, self-evolve **falls back gracefully** to existing manual checks for D2/D3/D4 (the current behavior), with a clear warning that automated validation is unavailable and a hint to install plugin-dev. This mirrors Phase 01's approach: "fail loud, offer fix." No dimension is skipped — the audit still runs, just without automation.

  ```
  [auto] plugin-dev availability — Selected: graceful fallback with warning (recommended default)
  Reason: Consistent with Phase 01's approach of blocking only at session start, warning at agent level.
  ```

### Performance and Context Budget (Cross-cutting)
- **D-06:** Sub-agent spawning (plugin-validator) happens **once per audit cycle** for D2+D3 combined (not per-file). This avoids spawning overhead for each file in the plugin. The plugin-validator receives all relevant files in a single spawn, runs validation, and returns results. Subsequent dimensions run inline after the sub-agent completes.

### Claude's Discretion
- Exact wording of warning/error messages in collision detection output
- Ordering of D2/D3 sub-agent spawn relative to other dimensions
- Whether D6 uses `plugin-dev:plugin-settings` or `plugin-dev:mcp-integration` (researcher to determine based on actual skill capabilities)
- Whether collision detection scans all globally installed plugins or only known-risk namespaces (researcher to determine based on performance analysis)

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project Planning
- `.planning/ROADMAP.md` — Phase 2 goal and 5 success criteria
- `.planning/REQUIREMENTS.md` — SE-01 through SE-08 with traceability
- `.planning/PROJECT.md` — Project constraints, plugin boundary, out-of-scope items
- `.planning/phases/01-foundation-risk-mitigation/01-CONTEXT.md` — Phase 1 decisions (D-01 through D-09) that Phase 2 must follow

### Self-Evolve Agent (current state)
- `.claude/agents/self-evolve.md` — Current 250-line agent with 7 manual audit dimensions (D1-D7), Dependency Check preamble, and superpowers:verification-before-completion skill

### Phase 1 Deliverables (integration context)
- `.claude/agents/bug-hunt.md` — Reference: preamble structure with Design note for skill-based routing
- `.claude/agents/impl.md` — Reference: preamble with agent spawn validation
- `.claude/scripts/validate-deps.py` — Reference: installed_plugins.json parsing pattern
- `~/.claude/plugins/installed_plugins.json` — Authoritative plugin presence source

### plugin-dev Plugin (external)
- `~/.claude/plugins/cache/claude-plugins-official/plugin-dev/` — Available skills (agent-development, skill-development, hook-development, plugin-structure, plugin-settings, command-development, mcp-integration) and plugin-validator agent
- `.claude/plugin.json` — TGOSKits plugin manifest (agents, commands, hooks)

## Existing Code Insights

### Reusable Assets
- **self-evolve D1-D7 audit loop** (`.claude/agents/self-evolve.md`): Existing cyclic audit structure with dimension labeling. New plugin-dev skills and sub-agent spawns slot into specific dimensions without restructuring the loop.
- **installed_plugins.json parser** (`.claude/scripts/validate-deps.py`): Already parses this file with `parse_version()` and `check_plugins()`. The D4 cross-reference check can reuse this parsing logic.
- **Dependency Check preamble** (all 6 agents): Standardized pattern from Phase 01 — self-evolve's updated preamble follows the same structure for listing its new plugin-dev dependencies.
- **Sub-agent spawn pattern** (`.claude/agents/impl.md`, `.claude/agents/pr-review.md`): Existing agent files that reference spawn targets in body text — self-evolve's plugin-validator spawn follows the same convention.

### Established Patterns
- **Agent frontmatter format:** YAML with `name`, `description`, `skills`, `tools` fields. New plugin-dev skills added to `skills:` list.
- **Skill reference format:** `plugin:skill-name` (e.g., `plugin-dev:skill-development`). Project-local skills have no prefix.
- **Phase 01 preamble standard:** `### Dependency Check` with Skills/Tools/Agents sections. self-evolve's updated preamble lists all new plugin-dev skills and the plugin-validator spawn target.
- **Graceful fallback pattern:** Phase 01's validate-deps.py returns clear error message with batch install command. self-evolve follows the same pattern for missing plugin-dev.

### Integration Points
- **self-evolve frontmatter:** `skills:` field updated with up to 5 plugin-dev skills (selective loading by dimension — exact list determined by researcher)
- **self-evolve body:** `### Dependency Check` preamble updated with new skills + plugin-validator spawn target
- **self-evolve D2/D3 sections:** Replaced with sub-agent spawn invocation; agent returns structured results
- **self-evolve D4 section:** Extended with installed_plugins.json cross-reference logic
- **installed_plugins.json:** Read at D4 time for cross-reference validation (not at preamble time — that's validate-deps.py's job)

## Specific Ideas

No specific references — open to standard approaches. The researcher should investigate each plugin-dev skill's actual capabilities and map them to the appropriate audit dimension.

## Deferred Ideas

None — discussion stayed within phase scope.

---

*Phase: 02-self-evolve-enhancement*
*Context gathered: 2026-05-14*
