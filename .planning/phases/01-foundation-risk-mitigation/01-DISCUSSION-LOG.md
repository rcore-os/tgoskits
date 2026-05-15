# Phase 1: Foundation (Risk Mitigation) - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-05-13
**Phase:** 01-foundation-risk-mitigation
**Areas discussed:** Dependency Documentation, Validation Mechanism, Validation Failure Behavior, Validation Scope

---

## Dependency Documentation (FND-01)

| Option | Description | Selected |
|--------|-------------|----------|
| Inline CLAUDE.md section | Add a 'Required Plugins' section to the existing project CLAUDE.md listing superpowers >= 5.1.0, pr-review-toolkit, code-review, plugin-dev with minimum versions | ✓ |
| CLAUDE.md + DEPENDENCIES.md | Brief mention in CLAUDE.md pointing to a new .claude/DEPENDENCIES.md with detailed version requirements | |
| CLAUDE.md + install guard | CLAUDE.md section + a startup check hook that validates required plugins are actually installed | |

**User's choice:** Inline CLAUDE.md section
**Notes:** Table format with install commands included. Self-contained for new contributors.

### Security-auditor handling

| Option | Description | Selected |
|--------|-------------|----------|
| Document as required | List code-modernization (security-auditor) as a required dependency | |
| Replace with available agent | Find an alternative security agent from already-installed plugins | |
| Defer, document as optional | Note that security-auditor is recommended but not required | |
| Remove entirely | Delete all security-auditor references from agents | ✓ |

**User's choice:** Remove entirely — no code-modernization dependency
**Notes:** User explicitly requested removal of code-modernization plugin dependency. All security-auditor spawn references to be deleted from pr-review, bug-hunt, impl, and driver-audit.

---

## Validation Mechanism (FND-02, FND-03)

| Option | Description | Selected |
|--------|-------------|----------|
| In-agent preamble | Each agent's markdown body starts with a validation step | |
| Pre-invoke hook | A Claude Code hook runs a validation script before agent invocation | |
| Both: hook + preamble | Hook catches missing deps early, preamble provides detailed error messages | ✓ |

**User's choice:** Both — hook for fast fail, preamble for detailed errors

### Hook implementation

| Option | Description | Selected |
|--------|-------------|----------|
| Shell script hook | A .claude/hooks/validate-deps.sh that checks plugin installation status via installed_plugins.json | ✓ |
| Markdown agent hook | A .claude/hooks/validate-deps.md that Claude Code executes | |

**User's choice:** Shell script hook
**Notes:** Fast, testable, runs before agent sees any context.

### Preamble style

| Option | Description | Selected |
|--------|-------------|----------|
| Minimal check block | Short standardized block at the top of each agent | ✓ |
| Agent-specific preamble | Custom validation logic per agent | |

**User's choice:** Minimal standardized check block
**Notes:** Same structure across all 6 agents for consistency.

---

## Validation Failure Behavior (FND-02, FND-03)

| Option | Description | Selected |
|--------|-------------|----------|
| Hard abort + instructions | Stop immediately with clear error message including exact install command | |
| Abort + fix option | Error message + offer to auto-run the install command | ✓ |
| Warn + degrade | Warn about missing deps but continue in degraded mode | |

**User's choice:** Abort + fix option
**Notes:** Batch fix all missing dependencies at once with a single consent prompt.

---

## Validation Scope (FND-02, FND-03)

| Option | Description | Selected |
|--------|-------------|----------|
| All three layers | Validate frontmatter skills:, tools:, AND spawned-agent references in body | ✓ |
| Skills + spawned agents | Validate frontmatter skills: and spawned-agent references only | |
| Skills only | Only validate frontmatter skills: references | |

**User's choice:** All three layers
**Notes:** Most thorough approach — no silent failures tolerated.

---

## Claude's Discretion

- Exact wording of error messages in the preamble block
- Specific shell commands used in the validation hook
- Ordering of checks within each validation layer

## Deferred Ideas

None — discussion stayed within phase scope.
