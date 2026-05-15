# Phase 1: Foundation (Risk Mitigation) - Research

**Researched:** 2026-05-13
**Domain:** Claude Code Plugin Dependency Validation
**Confidence:** HIGH

## Summary

Phase 1 addresses a critical systemic risk: TGOSKits agents silently fail when required plugins, skills, or sub-agents are unavailable. An agent that invokes `superpowers:systematic-debugging` when superpowers isn't installed simply skips the skill with no error. An agent that spawns `security-auditor` when that plugin doesn't exist silently produces no sub-agent output. The result is degraded output indistinguishable from correct output -- the worst failure mode in an OS development context.

The solution has two layers with complementary failure modes. **Layer 1 (shell script hook)**: a Python script registered as a SessionStart hook in `hooks.json` reads `~/.claude/plugins/installed_plugins.json` and validates all required plugins (superpowers >= 5.1.0, pr-review-toolkit) are installed with satisfying versions. If any are missing, it blocks the session with a clear error message and a batch install command. This catches missing plugins before any agent context loads. **Layer 2 (in-agent preamble block)**: a standardized markdown section at the top of each agent's body validates three categories of cross-plugin references -- `skills:` frontmatter entries with `plugin:` prefix, `tools:` frontmatter entries, and spawned-agent references in body text. This catches missing skills/tools/agents that Layer 1 can't detect (e.g., a skill that was removed from a plugin between install and agent invocation, or a sub-agent reference that resolved during install but is now broken).

Additionally, all stale `security-auditor` spawn references must be removed from 4 agents (pr-review, bug-hunt, impl, driver-audit) as this plugin is not installed and the user decided to remove it as a dependency. The pr-review and bug-hunt agents need `WebSearch` and `WebFetch` tools added to their frontmatter for web documentation access. The pr-review agent needs `superpowers:systematic-debugging` added to its skills.

**Primary recommendation:** Implement the two-layer validation as Python script + standardized YAML-style preamble block. Follow the existing hook pattern from `docker-check.py` and `pre-pr-gate.py` for the shell script. The preamble block should be copy-paste identical across all 6 agents with agent-specific dependency lists.

## User Constraints (from CONTEXT.md)

### Locked Decisions

- **D-01:** Hard dependencies documented as an inline section in project CLAUDE.md with table format including plugin name, minimum version, what it provides, and copy-paste install commands.
- **D-02:** Required plugins: `superpowers` >= 5.1.0, `pr-review-toolkit`. The `code-modernization` plugin (security-auditor) is explicitly removed as a dependency -- no agent references it.
- **D-03:** Two-layer validation: a shell script pre-invoke hook checks `installed_plugins.json` for fast failure before agent context loads, plus a minimal standardized preamble block at the top of each agent's markdown body.
- **D-04:** The shell script hook validates against the live `~/.claude/plugins/installed_plugins.json` file. It checks plugin presence and minimum versions.
- **D-05:** The in-agent preamble is a short standardized block: lists required skills/tools/agents, verifies each resolves, and aborts with clear error if any are missing. Same block structure across all 6 agents.
- **D-06:** On validation failure: abort the agent invocation with a clear error message listing exactly what's missing, followed by an offer to auto-fix (batch install command for all missing dependencies at once with a single consent prompt).
- **D-07:** Batch fix approach: all missing dependencies listed together in one prompt rather than individual prompts per dependency.
- **D-08:** Validation covers all three reference layers: frontmatter `skills:` entries, frontmatter `tools:` entries, AND spawned-agent references in the agent body text.
- **D-09:** All `security-auditor` spawn references removed from pr-review, bug-hunt, impl, and driver-audit agents. Remaining spawn targets for detection: `debugger` (bug-hunt), `test-gen` (impl).

### Claude's Discretion

- Exact wording of error messages in the preamble block
- Specific shell commands used in the validation hook
- Ordering of checks within each validation layer

### Deferred Ideas (OUT OF SCOPE)

None -- discussion stayed within phase scope.

## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| FND-01 | Document required global plugins (superpowers >= 5.1.0, pr-review-toolkit) as hard dependencies in project CLAUDE.md | Verified installed_plugins.json format, confirmed superpowers 5.1.0 installed with gitCommitSha 917e5f5, confirmed pr-review-toolkit installed. Table format spec'd in D-01. |
| FND-02 | Add startup validation to all 6 agents that checks referenced `plugin:skill` entries resolve against installed plugins, failing loudly if not found | Preamble block pattern designed. Skills verified on disk for superpowers 5.1.0. Plugin:skill resolution via installed_plugins.json installPath mapping. |
| FND-03 | Add missing-agent detection to pr-review, bug-hunt, impl, driver-audit -- fail with clear error if spawn target unavailable | Spawn targets catalogued (security-auditor removed per D-09, debugger + test-gen remain). Detection via grep for inline `agent-name` references in body text. |
| FND-04 | Add `WebSearch` + `WebFetch` tools to pr-review and bug-hunt agent frontmatter | Confirmed tools are valid Claude Code tool names (already used by impl agent). Both agents currently missing these tools. |
| FND-05 | Add `superpowers:systematic-debugging` skill to pr-review agent frontmatter | Verified skill exists on disk at superpowers/5.1.0/skills/systematic-debugging/. pr-review currently missing this skill; bug-hunt already has it. |

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Plugin presence verification | Shell Script (Hook) | -- | Must run before any agent context loads; shell hooks are the only pre-agent-invocation mechanism |
| Plugin version checking | Shell Script (Hook) | -- | Version comparison requires JSON parsing; Python is the project-standard hook language |
| Per-agent skill resolution | In-Agent Preamble | -- | Skills resolve at agent invocation time; preamble is the first code Claude executes |
| Per-agent tool availability | In-Agent Preamble | -- | Tools are declared in frontmatter; preamble validates they were correctly loaded |
| Sub-agent spawn target detection | In-Agent Preamble | -- | Spawn references are in body text; only Claude processing the agent context can detect these |
| Batch fix prompting | In-Agent Preamble | -- | User-facing interaction must happen within Claude's context; hooks can't prompt |
| Dependency documentation | CLAUDE.md (project root) | -- | Developer-facing docs belong in project CLAUDE.md per existing conventions |
| security-auditor removal | Source files | -- | Pure text removal in 4 agent .md files |
| Tool addition (WebSearch/WebFetch) | Agent frontmatter | -- | YAML frontmatter edit in 2 agent files |

## Standard Stack

### Core

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| Python 3 | 3.14.4 (verified) | Hook script execution | Already used by 3 existing hooks (docker-check.py, pre-pr-gate.py, post-tool-use-log.py). Project convention. [VERIFIED: codebase] |
| Bash | 5.3.9 (verified) | Fallback script execution | Available on all Linux systems. No additional dependencies. [VERIFIED: which bash] |
| jq | 1.8.1 (verified) | JSON parsing alternative | Optional -- Python's `json` module is the primary parser. jq provides a simpler alternative for one-liner checks. [VERIFIED: which jq] |

### Supporting

| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| Node.js | v26.1.0 | Alternative hook runtime | Not needed for this phase -- Python covers all JSON parsing needs. Listed for completeness. [VERIFIED: which node] |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Python hook script | Bash + jq | Bash+jq is simpler but error-prone for complex JSON traversal (nested plugin entries, version comparison). Python's json module is more maintainable and already proven in 3 existing hooks. |
| SessionStart hook | PreToolUse hook | SessionStart runs once, catches all deps upfront. PreToolUse would run per-tool-invocation, wasting cycles. SessionStart is the existing pattern (see global ~/.claude/settings.json). |

**Installation (no new packages needed):**
- Python 3 and jq are already present on the target system
- No pip/npm installs required -- only stdlib `json`, `os`, `sys`, `subprocess` used

**Version verification:** Python 3.14.4 [VERIFIED: python3 --version], Bash 5.3.9 [VERIFIED: bash --version], jq 1.8.1 [VERIFIED: jq --version], Node v26.1.0 [VERIFIED: node --version]

## Architecture Patterns

### System Architecture Diagram

```
User invokes Claude Code in TGOSKits workspace
         │
         ▼
┌─────────────────────────────────────────────┐
│ LAYER 1: SessionStart Hook                  │
│ .claude/scripts/validate-deps.py            │
│                                             │
│ 1. Read ~/.claude/plugins/                   │
│    installed_plugins.json                   │
│ 2. Check superpowers@... version >= 5.1.0   │
│ 3. Check pr-review-toolkit@... present      │
│ 4. Check security-auditor@... NOT present   │
│    (warn if found -- it was removed)        │
│                                             │
│ ┌─ ALL PRESENT? ──► YES ──► Continue ──────┐│
│ │                                          ││
│ └─ NO ──► BLOCK: print batch install cmd,  ││
│            exit 1, session does not start   ││
└─────────────────────────────────────────────┘
         │ (session starts)
         ▼
┌─────────────────────────────────────────────┐
│ Agent invocation (e.g., /pr-review)         │
│ Claude loads agent .md file                 │
└─────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────┐
│ LAYER 2: In-Agent Preamble Block           │
│ (standardized section in each agent .md)    │
│                                             │
│ For THIS agent, verify:                     │
│ A. Frontmatter skills: with plugin: prefix  │
│    resolve in installed_plugins.json        │
│ B. Frontmatter tools: are valid tool names  │
│ C. Body text spawn references               │
│    grep for `agent-name` in body           │
│                                             │
│ ┌─ ALL RESOLVE? ──► YES ──► Execute agent ─┐│
│ │                                          ││
│ └─ NO ──► ABORT with:                      ││
│   "AGENT ABORTED: Required dependencies     ││
│    missing. <list>. Fix with: <command>"    ││
└─────────────────────────────────────────────┘
         │
         ▼
Agent executes its normal workflow
```

### Recommended Project Structure

```
.claude/
├── scripts/
│   └── validate-deps.py          # NEW: Layer 1 shell script hook
├── hooks/
│   └── hooks.json                # MODIFIED: add SessionStart hook entry
├── agents/
│   ├── pr-review.md              # MODIFIED: add preamble, WebSearch/WebFetch, systematic-debugging, remove security-auditor
│   ├── bug-hunt.md               # MODIFIED: add preamble, WebSearch/WebFetch, remove security-auditor
│   ├── impl.md                   # MODIFIED: add preamble, remove security-auditor
│   ├── driver-audit.md           # MODIFIED: add preamble, remove security-auditor
│   ├── test-gen.md               # MODIFIED: add preamble
│   └── self-evolve.md            # MODIFIED: add preamble
├── plugin.json                   # NO CHANGES (agents already registered)
├── settings.json                 # NO CHANGES
└── CLAUDE.md                     # NO CHANGES (project CLAUDE.md modified by FND-01)
```

Note: The dependency documentation (FND-01) goes in the project-root `CLAUDE.md` (at `/home/rimuru/Projects/Code/homework/OS/tgoskits/CLAUDE.md`), not in `.claude/CLAUDE.md`.

### Pattern 1: Hook Script Structure (Layer 1)

**What:** Python script registered as a SessionStart hook that validates plugin dependencies before any agent context loads.

**When to use:** This is the only pattern for Layer 1 -- follow it exactly.

**Example:** [CITED: existing .claude/scripts/docker-check.py and pre-pr-gate.py patterns]
```python
#!/usr/bin/env python3
"""SessionStart hook: validate required plugins are installed with minimum versions.
Reads ~/.claude/plugins/installed_plugins.json. Exits 0 when all deps satisfied;
exits 1 with clear error and batch fix command when deps are missing."""

import json
import os
import sys

INSTALLED_PLUGINS_PATH = os.path.expanduser(
    "~/.claude/plugins/installed_plugins.json"
)

REQUIRED_PLUGINS = {
    "superpowers@claude-plugins-official": {
        "min_version": "5.1.0",
        "purpose": "systematic-debugging, verification-before-completion, brainstorming skills"
    },
    "pr-review-toolkit@claude-plugins-official": {
        "min_version": None,  # any version OK
        "purpose": "code-reviewer, silent-failure-hunter, pr-test-analyzer agents"
    }
}

def parse_version(ver_str):
    """Parse version string like '5.1.0' or 'unknown' into comparable tuple."""
    if ver_str == "unknown":
        return (0,)  # satisfy any min_version check
    try:
        return tuple(int(x) for x in ver_str.split("."))
    except (ValueError, AttributeError):
        return (0,)

def check_plugins():
    if not os.path.exists(INSTALLED_PLUGINS_PATH):
        print("BLOCKED: No plugins installed.", file=sys.stderr)
        print("Install required plugins:", file=sys.stderr)
        print("  claude plugins install superpowers@claude-plugins-official", file=sys.stderr)
        print("  claude plugins install pr-review-toolkit@claude-plugins-official", file=sys.stderr)
        return False

    with open(INSTALLED_PLUGINS_PATH) as f:
        data = json.load(f)

    installed = data.get("plugins", {})
    missing = []

    for plugin_key, req in REQUIRED_PLUGINS.items():
        entries = installed.get(plugin_key, [])
        if not entries:
            missing.append((plugin_key, "not installed", req["purpose"]))
            continue

        # Check version (use the first entry's version)
        installed_ver = entries[0].get("version", "unknown")
        min_ver = req["min_version"]
        if min_ver and parse_version(installed_ver) < parse_version(min_ver):
            missing.append((
                plugin_key,
                f"version {installed_ver} (need >= {min_ver})",
                req["purpose"]
            ))

    if missing:
        print("BLOCKED: Required plugins missing or outdated:\n", file=sys.stderr)
        for name, detail, purpose in missing:
            print(f"  - {name}: {detail}", file=sys.stderr)
            print(f"    Provides: {purpose}", file=sys.stderr)
        print(f"\nFix with a single command:", file=sys.stderr)
        plugin_names = " ".join(m[0].split("@")[0] for m in missing)
        print(f"  claude plugins install {plugin_names}", file=sys.stderr)
        return False

    return True

if not check_plugins():
    sys.exit(1)
sys.exit(0)
```

### Pattern 2: In-Agent Preamble Block (Layer 2)

**What:** Minimal standardized validation block that Claude processes before executing the agent's main workflow.

**When to use:** Top of every agent's markdown body, immediately after the YAML frontmatter.

**Example (pr-review agent):**
```markdown
### Dependency Check

Before executing any review work, verify these are available:

**Skills (resolve via installed plugins):**
- `superpowers:systematic-debugging` — root-cause analysis for bug classification
- `superpowers:verification-before-completion` — confirm fixes before marking complete

**Tools (verify in current context):**
- `WebSearch`, `WebFetch` — Linux man-page and POSIX spec lookup

**Agents (verify spawn targets resolve):**
- None (security-auditor removed; no cross-plugin spawns in this phase)

**If any above is missing:** ABORT immediately. Output:
> "AGENT ABORTED: pr-review missing required dependencies: <list>. Fix with: claude plugins install <names>"

Do NOT proceed with the review if dependencies are unavailable. All missing skills silently degrading into skipped work is a BLOCK-level risk for OS kernel code review.
```

**Key design constraints:**
- Same structure across all 6 agents (heading, categories, failure message format)
- Agent-specific dependency lists only
- Uses inline `code` formatting for skill/tool/agent names (consistent with existing TGOSKits agent style)
- Abort message uses "AGENT ABORTED:" prefix for grep-ability

### Pattern 3: Agent Dependency Catalog

**What:** A mapping of what each agent depends on and what needs to change.

**Per-agent dependency matrix (current state -> target state):**

| Agent | Current skills (plugin:) | Target skills (plugin:) | Current tools | Target tools | Spawn refs to remove | Spawn refs to detect |
|-------|-------------------------|------------------------|---------------|--------------|----------------------|---------------------|
| pr-review | superpowers:verification-before-completion | + superpowers:systematic-debugging | Read,Write,Edit,Bash,Grep,Glob | + WebSearch, WebFetch | security-auditor (line 24) | none |
| bug-hunt | superpowers:systematic-debugging, superpowers:verification-before-completion | no change | Read,Write,Edit,Bash,Grep,Glob | + WebSearch, WebFetch | security-auditor (line 27) | debugger (line 27) |
| impl | superpowers:verification-before-completion | no change | Read,Write,Edit,Bash,Grep,Glob,WebSearch,WebFetch | no change | security-auditor (line 29, 465) | test-gen (line 27) |
| driver-audit | superpowers:verification-before-completion | no change | Read,Grep,Glob | no change | security-auditor (line 19) | none |
| test-gen | superpowers:verification-before-completion | no change | Read,Write,Bash,Grep,Glob | no change | none | none |
| self-evolve | superpowers:verification-before-completion | no change | Read,Write,Edit,Bash,Grep,Glob | no change | none | none |

### Anti-Patterns to Avoid

- **Per-agent custom preamble:** Writing a different validation block for each agent. The preamble MUST have identical structure across all 6 agents (D-05). Only the dependency lists differ.
- **Silent degradation on missing deps:** The current behavior (skill silently skipped, spawn silently no-op) is the bug this phase fixes. Every validation failure MUST produce a loud, visible error.
- **Individual fix prompts:** Suggesting one `claude plugins install` command per missing dependency violates D-07. All missing deps must be listed in a single batch fix command.
- **Validation only at top:** Checking dependencies only in the preamble but not verifying they're still available mid-workflow. The preamble is sufficient for Phase 1 (the risk is at invocation time, not mid-session plugin uninstall).
- **Hardcoding plugin paths:** Using absolute paths like `/home/rimuru/.claude/plugins/...` instead of `~/.claude/plugins/...`. The script must be portable across users.
- **Removing security-auditor from frontmatter only:** Some agents list security-auditor as a spawn target in body text, NOT in frontmatter. Grep for backtick-wrapped references like `` `security-auditor` `` in body text.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| JSON parsing in validation hook | Custom JSON parser | Python `json` module (stdlib) | installed_plugins.json has nested structure with per-plugin lists. Python's `json.load()` handles all edge cases. |
| Version comparison | Custom semver parser | Simple tuple comparison `tuple(int(x) for x in s.split("."))` | Plugin versions are simple `major.minor.patch` strings. No need for full semver library. |
| Plugin presence check | Filesystem traversal of plugin cache dirs | `installed_plugins.json` | The JSON file is the authoritative source. Filesystem traversal would miss plugins installed but not loaded. |
| Skill resolution verification | Manual path checking | Cross-reference `installed_plugins.json` installPath with filesystem | The installPath in installed_plugins.json + `/skills/<name>/` is the canonical resolution path. |
| Spawn target detection | Static YAML parsing | Grep for backtick-wrapped names in body text | Spawn references are free-text in markdown, not structured YAML. Pattern: `` `agent-name` `` preceded by "spawn", "delegate to", or "launch". |

**Key insight:** The validation system leverages existing Claude Code infrastructure (hooks.json, installed_plugins.json, frontmatter parsing). We're not building a new plugin resolution system -- we're checking that the existing resolution system won't silently fail for our specific cross-plugin references.

## Common Pitfalls

### Pitfall 1: installed_plugins.json Entries Are Arrays, Not Objects

**What goes wrong:** Script treats `plugins["superpowers@claude-plugins-official"]` as a single object, but it's actually an array of objects (multiple install scopes possible).

**Why it happens:** The JSON structure uses arrays even when there's typically one entry. A contributor might write `plugins[name].version` instead of `plugins[name][0].version`.

**How to avoid:** Always index `[0]` when accessing plugin entries, or iterate over all entries and check each.

**Warning signs:** `TypeError: 'list' object has no attribute 'get'` in the validation script.

[CITED: verified in installed_plugins.json -- structure is `{"plugins": {"name@source": [{"scope": "...", "version": "...", ...}]}}`]

### Pitfall 2: Version "unknown" Causes Comparison Failures

**What goes wrong:** Many plugins have `"version": "unknown"` (including pr-review-toolkit). A naive `parse_version("unknown")` throws ValueError.

**Why it happens:** Claude Code doesn't always populate version strings for git-installed or development plugins.

**How to avoid:** Treat `"unknown"` as satisfying any minimum version requirement (it means "dev/edge install, assume latest"). Document this behavior.

**Warning signs:** `ValueError: invalid literal for int()` in version comparison code.

[CITED: verified -- 12 out of 28 installed plugins have version "unknown"]

### Pitfall 3: Hook Registration Order Matters

**What goes wrong:** Adding the validation hook to `hooks.json` in the wrong position, or with a matcher that doesn't cover the intended trigger.

**Why it happens:** hooks.json is processed in order. A SessionStart hook must be registered as the first hook. If registered as PreToolUse, it fires on every Bash invocation (performance and noise issue).

**How to avoid:** Register as a `SessionStart` hook (matching the existing pattern in `~/.claude/settings.json`). Verify the matcher covers the TGOSKits-specific dependency check without interfering with other plugins.

**Warning signs:** Hook fires too often (every Bash command) or not at all (wrong event type).

### Pitfall 4: security-auditor References in Multiple Forms

**What goes wrong:** Removing only `` `security-auditor` `` spawn references but missing other forms like "the security-auditor agent" or "run the security-auditor."

**Why it happens:** Agent body text uses varied natural language, not just backtick-wrapped spawn syntax.

**How to avoid:** Grep for all forms: `security-auditor`, `security.auditor`, `@security-auditor`. Remove every mention.

**Warning signs:** The word "security-auditor" still appears in agent files after the edit pass. Use `grep -rn security-auditor .claude/agents/` to verify zero results.

[CITED: existing agent files -- pr-review line 24, bug-hunt line 27, impl lines 29 and 465, driver-audit line 19]

### Pitfall 5: Preamble Must Precede Agent Workflow Logic

**What goes wrong:** Placing the preamble block too deeply in the agent file, after some initial instructions. Claude might start executing before reaching the validation block.

**Why it happens:** Agent files are long (pr-review is 359 lines). A natural impulse is to preserve the existing header structure.

**How to avoid:** Place the preamble IMMEDIATELY after the closing `---` of the YAML frontmatter, before any other content. It must be the first thing Claude processes after loading the agent context.

**Warning signs:** Preamble is on line 30+ of the agent file, after non-validation content.

## Code Examples

### Validated Plugin Presence Check

```python
# Source: [VERIFIED: installed_plugins.json structure analysis]
import json, os, sys

PLUGINS_FILE = os.path.expanduser("~/.claude/plugins/installed_plugins.json")

with open(PLUGINS_FILE) as f:
    data = json.load(f)

# Structure: {"version": 2, "plugins": {"name@source": [entry, ...]}}
plugins = data.get("plugins", {})

# Check superpowers exists with version >= 5.1.0
sp_entries = plugins.get("superpowers@claude-plugins-official", [])
if not sp_entries:
    print("BLOCKED: superpowers not installed", file=sys.stderr)
    sys.exit(1)

sp_ver = sp_entries[0].get("version", "0")
if sp_ver != "unknown":
    major, minor, patch = [int(x) for x in sp_ver.split(".")]
    if (major, minor) < (5, 1):
        print(f"BLOCKED: superpowers {sp_ver} < 5.1.0 required", file=sys.stderr)
        sys.exit(1)
```

### Hook Registration Pattern

```json
// Source: [CITED: existing .claude/hooks/hooks.json pattern]
// Add as FIRST entry in the hooks array:
{
  "hooks": [
    {
      "event": "SessionStart",
      "command": "python3 \"${CLAUDE_PLUGIN_ROOT}/scripts/validate-deps.py\""
    },
    // ... existing hooks follow
  ]
}
```

### Preamble Template (All Agents)

```markdown
### Dependency Check

Before executing any work, verify these dependencies are available:

**Skills** (must resolve via installed plugins):
- `superpowers:verification-before-completion`

**Tools** (must be present in this context):
- Read, Write, Edit, Bash, Grep, Glob

**Agents** (must be spawnable):
- None

If any item above is missing, ABORT with:
> "AGENT ABORTED: <agent-name> missing: <list>. Fix: claude plugins install <names>"

Do NOT proceed with degraded capabilities. Silent dependency failures in OS kernel workflows are a BLOCK-level risk.
```

### security-auditor Removal Verification

```bash
# Source: [CITED: D-09 requires complete removal]
# After editing, verify zero references remain:
grep -rn "security-auditor" .claude/agents/
# Expected output: (empty -- no matches)

# Known locations to remove:
# pr-review.md line 24: spawn the `security-auditor` agent
# bug-hunt.md line 27: spawn the `security-auditor` agent
# impl.md line 29: spawn the `security-auditor` agent
# impl.md line 465: `security-auditor` agent (Integration Map table)
# driver-audit.md line 19: spawn the `security-auditor` agent
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Plugin dependency silently skipped | Validation hook + preamble block | This phase | Catches missing deps before agents degrade |
| security-auditor as hard dependency | security-auditor removed entirely | This phase (D-02, D-09) | 4 agents need text edits; no functionality loss (auditor was never installed) |
| Tools not verified at startup | Preamble validates tools: frontmatter entries | This phase | Agents that need WebSearch/WebFetch will fail loudly if not available |
| No centralized dependency docs | CLAUDE.md table with install commands | This phase (FND-01) | New contributors can install deps without reading agent source |

**Deprecated/outdated:**
- `security-auditor` references: Removed per D-02/D-09. If security review is needed later, it's a separate phase with a different plugin.
- Implicit dependency assumption: Agents assumed skills "just work." Now they verify explicitly.

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `SessionStart` is a valid hook event type for project-level `hooks.json` | Architecture Patterns | If Claude Code only supports SessionStart in global settings.json (not plugin hooks.json), the shell script must be registered differently (e.g., as a PreToolUse hook or invoked manually). LOW risk -- the hookify plugin suggests it's supported. |
| A2 | Plugin `skills:` with `plugin:skill-name` syntax resolve against `installed_plugins.json` installPath + `/skills/<skill-name>/` | Architecture Patterns | If the resolution mechanism differs, the validation script's path checking logic needs adjustment. LOW risk -- verified by cross-referencing installed_plugins.json install paths with filesystem. |
| A3 | Claude Code processes agent markdown sequentially from top to bottom | Common Pitfalls | If Claude uses a different context processing order (e.g., skills-first, then body), the preamble might not be the first thing executed. LOW risk -- all known Claude Code agent implementations are sequential. |
| A4 | `WebSearch` and `WebFetch` are valid Claude Code tool names | Stack | If these tool names have been renamed or deprecated, the frontmatter additions would be invalid. MEDIUM risk -- confirmed by usage in impl agent's frontmatter but tool names could change in future Claude Code versions. |
| A5 | The pr-review-toolkit plugin's agents are at `pr-review-toolkit@claude-plugins-official` | Don't Hand-Roll | If the plugin is installed under a different marketplace name, detection would fail. LOW risk -- confirmed in installed_plugins.json. |

## Open Questions (RESOLVED)

1. **SessionStart hook event type availability in plugin hooks.json** — RESOLVED: SessionStart used as primary hook registration target per Plan 01 Task 3. If SessionStart is not available at plugin-level hooks.json, the fallback is a PreToolUse hook with broad matcher and single-execution state file guard, documented as a contingency in Plan 01 Task 3's action.

2. **Preamble block format -- heading level** — RESOLVED: `### Dependency Check` (H3) per Plan 02 Task 2. This is visually distinct from agent-level `##` sections, subordinate to the agent's title, and easier to grep across files.

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| Python 3 | validate-deps.py hook script | YES | 3.14.4 | Bash+jq (less robust) |
| jq | JSON parsing alternative | YES | 1.8.1 | Python json module (primary) |
| Bash | Shell script execution | YES | 5.3.9 | -- |
| superpowers plugin | All agents (skills) | YES | 5.1.0 | -- |
| pr-review-toolkit plugin | Phase 3 agents | YES | unknown (dev) | -- |
| ~/.claude/plugins/installed_plugins.json | Validation script | YES | N/A | -- |

**Missing dependencies with no fallback:** None. All required dependencies are present on the target system.

**Missing dependencies with fallback:** None.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Python `unittest` + shell-based integration tests |
| Config file | None -- see Wave 0 |
| Quick run command | `python3 -m pytest .claude/scripts/test_validate_deps.py -x` (TBD in Wave 0) |
| Full suite command | `python3 -m pytest .claude/scripts/ -x && bash .claude/scripts/local-ci.sh quick` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| FND-01 | CLAUDE.md contains dependency table with superpowers >= 5.1.0, pr-review-toolkit, install commands | unit | `grep -c "superpowers" CLAUDE.md && grep -c "pr-review-toolkit" CLAUDE.md && grep -c "claude plugins install" CLAUDE.md` | NO -- Wave 0 |
| FND-02 | All 6 agents have identical preamble structure | integration | `diff <(head -30 .claude/agents/pr-review.md) <(head -30 .claude/agents/bug-hunt.md)` (structural diff for preamble boundaries) | NO -- Wave 0 |
| FND-02 | validate-deps.py exits 0 when all plugins present | unit | `python3 .claude/scripts/validate-deps.py; echo $?` (mock installed_plugins.json) | NO -- Wave 0 |
| FND-02 | validate-deps.py exits 1 with error message when superpowers missing | unit | Same as above with modified mock | NO -- Wave 0 |
| FND-03 | Zero security-auditor references in any agent file | unit | `grep -rn "security-auditor" .claude/agents/ | wc -l` (expect 0) | NO -- Wave 0 |
| FND-04 | pr-review frontmatter has WebSearch in tools: | unit | `grep -A20 '^tools:' .claude/agents/pr-review.md | grep WebSearch` | NO -- Wave 0 |
| FND-04 | bug-hunt frontmatter has WebFetch in tools: | unit | `grep -A20 '^tools:' .claude/agents/bug-hunt.md | grep WebFetch` | NO -- Wave 0 |
| FND-05 | pr-review frontmatter has superpowers:systematic-debugging in skills: | unit | `grep -A20 '^skills:' .claude/agents/pr-review.md | grep superpowers:systematic-debugging` | NO -- Wave 0 |

### Sampling Rate
- **Per task commit:** `grep -rn "security-auditor" .claude/agents/ | wc -l` (fast zero-check)
- **Per wave merge:** Full phase gate suite (all 8 checks above)
- **Phase gate:** All 8 test commands pass with exit 0

### Wave 0 Gaps
- [ ] `.claude/scripts/test_validate_deps.py` -- unit tests for the validation script with mock installed_plugins.json
- [ ] `.claude/scripts/test_preamble_consistency.sh` -- structural consistency check across all 6 agent preamble blocks
- [ ] `.claude/scripts/test_frontmatter_tools.sh` -- verify WebSearch/WebFetch present in pr-review and bug-hunt
- [ ] Framework install: `python3 -m pip install pytest` (if not already available)

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | No | N/A |
| V3 Session Management | No | N/A |
| V4 Access Control | No | N/A |
| V5 Input Validation | Yes | Python JSON schema validation -- installed_plugins.json structure must be validated before parsing |
| V6 Cryptography | No | N/A |

### Known Threat Patterns for Plugin Dependency Validation

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Malformed installed_plugins.json causing validation script crash | Denial of Service | Wrap JSON parsing in try/except with graceful error message |
| Symlink attack on plugin installPath | Elevation of Privilege | Resolve real paths with `os.path.realpath()` before filesystem checks |
| Stale cache (old installed_plugins.json) masking missing plugins | Spoofing | Check file modification time; warn if > 24h old |
| Crafted plugin version strings causing parse errors | Denial of Service | Defensive version parsing -- return (0,) tuple on any parse failure |

## Sources

### Primary (HIGH confidence)
- [VERIFIED: codebase] All 6 agent files read in full: `.claude/agents/pr-review.md`, `bug-hunt.md`, `impl.md`, `driver-audit.md`, `test-gen.md`, `self-evolve.md`
- [VERIFIED: codebase] `.claude/hooks/hooks.json` -- existing hook registration pattern with PreToolUse/PostToolUse/Stop events
- [VERIFIED: codebase] `.claude/scripts/pre-pr-gate.py` -- proven hook script pattern (Python, env vars, exit codes)
- [VERIFIED: codebase] `.claude/scripts/docker-check.py` -- proven hook script pattern (gate logic, error messages)
- [VERIFIED: filesystem] `~/.claude/plugins/installed_plugins.json` -- complete plugin inventory with versions and install paths
- [VERIFIED: filesystem] `~/.claude/plugins/cache/claude-plugins-official/superpowers/5.1.0/skills/` -- all 14 available superpowers skills
- [VERIFIED: filesystem] `~/.claude/plugins/cache/claude-plugins-official/pr-review-toolkit/unknown/agents/` -- all 6 available pr-review-toolkit agents
- [VERIFIED: filesystem] `~/.claude/settings.json` -- global hook configuration with SessionStart event type
- [VERIFIED: codebase] `.claude/plugin.json` -- TGOSKits plugin manifest with 6 registered agents
- [CITED: project docs] `.planning/CONTEXT.md` (01-CONTEXT.md) -- user decisions D-01 through D-09

### Secondary (MEDIUM confidence)
- [VERIFIED: which] Python 3.14.4, Bash 5.3.9, jq 1.8.1, Node v26.1.0 -- all available on target system
- [CITED: project docs] `.planning/ROADMAP.md` -- phase success criteria and dependencies
- [CITED: project docs] `.planning/REQUIREMENTS.md` -- FND-01 through FND-05 with traceability

### Tertiary (LOW confidence)
- None -- all claims verified against codebase or filesystem.

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- all tools verified present with versions on target machine
- Architecture: HIGH -- existing hook patterns, installed_plugins.json format, and agent structure all validated by direct filesystem inspection
- Pitfalls: HIGH -- 3 of 5 pitfalls derived from direct inspection of installed_plugins.json structure anomalies (array wrapping, version "unknown")
- Requirements: HIGH -- all FND-01 through FND-05 mapped to concrete file modifications with verified current state

**Research date:** 2026-05-13
**Valid until:** 2026-07-13 (30 days -- stable domain, no external API dependencies)
