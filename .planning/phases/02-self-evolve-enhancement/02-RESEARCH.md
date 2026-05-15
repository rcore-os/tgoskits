# Phase 2: Self-Evolve Enhancement - Research

**Researched:** 2026-05-14
**Domain:** Claude Code agent enhancement, plugin-dev integration, sub-agent spawning
**Confidence:** HIGH

## Summary

This phase enhances the self-evolve agent to replace 4 of its 7 manual audit dimensions (D2, D3, D4, D6, D7) with automated checks via plugin-dev skills and sub-agent delegation, adds 2 superpowers skills for improvement ideation and root-cause analysis, and adds optional agent-name collision detection.

**Primary recommendation:** Load 5 plugin-dev skills + 2 superpowers skills into self-evolve frontmatter, spawn `plugin-dev:plugin-validator` as a sub-agent for D2+D3 combined, and extend D4 cross-reference to scan installed_plugins.json. The plugin-validator exists and handles both syntax and frontmatter validation. Skills map cleanly to audit dimensions. No agent-name collisions exist in the current global install set, but the detection mechanism should be built for future-proofing.

**Key finding:** There are ZERO agent-name collisions between TGOSKits' 6 agents and all globally installed plugin agents. The risky names `impl`, `test-gen`, `pr-review` do not conflict with any global agent. The collision detection mechanism is still valuable as a quality-of-life feature.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

- **D-01:** self-evolve spawns `plugin-dev:plugin-validator` as a **sub-agent** (via Agent tool) for D2 (Syntax) and D3 (Frontmatter) checks.
- **D-02:** D4 validates **all plugin:skill references across all agent files** (`.claude/agents/*.md`), not just self-evolve's own references. The check reads `~/.claude/plugins/installed_plugins.json` as the authoritative source.
- **D-03:** plugin-dev skills are loaded **selectively by audit dimension** rather than all at once. Five plugin-dev skills loaded: plugin-structure (D1), skill-development (D3), agent-development (D4), plugin-settings (D6), hook-development (D7).
- **D-04:** Collision detection is **simple grep-based**: scan globally installed plugins' agent names and warn on exact-name matches with TGOSKits agents. Result is a **warning** (non-blocking) surfaced in the D4 report. Feature flag remains "optional".
- **D-05:** If plugin-dev is not installed, self-evolve **falls back gracefully** to existing manual checks with a clear warning and installation hint.
- **D-06:** Sub-agent spawning (plugin-validator) happens **once per audit cycle** for D2+D3 combined (not per-file).

### Claude's Discretion
- Exact wording of warning/error messages in collision detection output
- Ordering of D2/D3 sub-agent spawn relative to other dimensions
- Whether D6 uses `plugin-dev:plugin-settings` or `plugin-dev:mcp-integration` (researcher to determine)
- Whether collision detection scans all globally installed plugins or only known-risk namespaces (researcher to determine)

### Deferred Ideas (OUT OF SCOPE)
- None — discussion stayed within phase scope.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| SE-01 | self-evolve spawns plugin-validator for D2+D3 | plugin-validator agent exists at `plugin-dev/agents/plugin-validator.md`; designed for comprehensive validation; spawn pattern confirmed |
| SE-02 | D4 cross-reference validates plugin:skill against installed_plugins.json | installed_plugins.json format confirmed (v2, plugin keyed by `name@marketplace`); validate-deps.py provides parsing pattern |
| SE-03 | load plugin-dev:skill-development for D3 quality review | skill-development SKILL.md confirmed (640 lines); covers skill frontmatter, writing style, progressive disclosure, validation |
| SE-04 | load plugin-dev:agent-development for D4 quality review | agent-development SKILL.md confirmed (400 lines); covers agent structure, frontmatter, system prompts, triggering conditions |
| SE-05 | load plugin-dev:hook-development for D7 hook validation | hook-development SKILL.md confirmed (710 lines); covers hook events, configuration, scripts, validation |
| SE-06 | optional agent-name collision detection against globally installed plugins | 0 actual collisions found; mechanism scans plugin cache agents/ dirs; warning-only severity |
| SE-07 | load superpowers:brainstorming for improvement ideation | brainstorming SKILL.md confirmed (9-phase process: explore, clarify, propose, design, validate) |
| SE-08 | load superpowers:systematic-debugging for root-cause analysis | systematic-debugging SKILL.md confirmed (Iron Law: no fixes without root cause investigation) |
</phase_requirements>

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| D2 syntax validation | Sub-agent (plugin-validator) | self-evolve fallback | plugin-validator agent handles structured validation in own context; self-evolve falls back to manual checks if unavailable |
| D3 frontmatter validation | Sub-agent (plugin-validator) | self-evolve + skill-development skill | plugin-validator validates YAML frontmatter; skill-development skill reviews skill quality in parallel |
| D4 cross-reference check | self-evolve (inline) | plugin-dev:agent-development | D4 logic stays inline in self-evolve; agent-development skill provides quality review framework |
| D7 hook validation | self-evolve (inline) | plugin-dev:hook-development | Hook checks stay inline; skill provides reference for hook structure patterns |
| Collision detection | self-evolve (inline, D4 pass) | — | Simple grep-based scan; no external skill needed |
| Improvement ideation | self-evolve (workflow) | superpowers:brainstorming | New workflow phase before audit rounds; brainstorming skill provides methodology |
| Root-cause analysis | self-evolve (workflow) | superpowers:systematic-debugging | Applied during D2-D7 when quality issues found |

## Standard Stack

### Core

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| plugin-dev | 1a2f18b05cf5 | Plugin development toolkit: 7 skills, 3 agents | Provides plugin-validator sub-agent + 5 audit-specific skills |
| superpowers | 5.1.0 | Power-user skills: 13 skills for advanced workflows | Provides brainstorming + systematic-debugging skills |
| plugin-dev:plugin-validator | (included) | Comprehensive plugin structure/syntax/frontmatter validation | Built-in agent designed for plugin validation; used as sub-agent for D2+D3 |

**Verified versions:**
```
plugin-dev@claude-plugins-official: 1a2f18b05cf5 [VERIFIED: installed_plugins.json]
superpowers@claude-plugins-official: 5.1.0 [VERIFIED: installed_plugins.json]
```

### Skills to Load in self-evolve Frontmatter

| Skill | Audit Dimension | Purpose | Why |
|-------|----------------|---------|-----|
| `plugin-dev:plugin-structure` | D1 (Paths) | Verify file/directory layout | Covers manifest structure, naming conventions, auto-discovery |
| `plugin-dev:skill-development` | D3 (Frontmatter) | Skill quality review | Covers writing style, progressive disclosure, frontmatter validation |
| `plugin-dev:agent-development` | D4 (Cross-ref) | Agent quality review | Covers agent structure, frontmatter, system prompt validation |
| `plugin-dev:plugin-settings` | D6 (Integration) | Settings/config consistency | Covers `.local.md` pattern, settings parsing, config validation |
| `plugin-dev:hook-development` | D7 (Hooks) | Hook structure validation | Covers hook events, configuration, scripts, validation utilities |
| `superpowers:brainstorming` | (new workflow) | Improvement ideation | 9-phase design process for identifying plugin improvements |
| `superpowers:systematic-debugging` | D2-D7 cross-cut | Root-cause analysis | Iron Law investigation process for quality issues |

**Total new skills in frontmatter: 7** (plus existing `superpowers:verification-before-completion`)

### Skills NOT to Load (and why)

| Skill | Reason Skipped |
|-------|---------------|
| `plugin-dev:command-development` | Slash command creation — not relevant to any audit dimension [VERIFIED: SKILL.md focused on command frontmatter, args, bash execution] |
| `plugin-dev:mcp-integration` | TGOSKits has no MCP servers; `.mcp.json` does not exist; `settings.json` is empty `{}` [VERIFIED: project files] |
| `plugin-dev:plugin-validator` | This is an AGENT (not a skill) — spawned as sub-agent for D2+D3, not loaded as a skill [VERIFIED: plugin-validator.md has name/model/color/tools frontmatter] |

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| plugin-validator sub-agent | Manual `python3 -m json.tool` + `sed` parsing (current) | Manual is less thorough, misses edge cases, consumes self-evolve's context |
| plugin-dev:plugin-settings for D6 | plugin-dev:mcp-integration for D6 | mcp-integration is about external MCP servers — irrelevant when project has none [ASSUMED: correct skill for config consistency is plugin-settings] |
| Full scan all plugins for collision | Scan only known-risk namespaces | Full scan is cheap (grep over <30 agent files); no performance concern |

## Architecture Patterns

### System Architecture Diagram

```
Self-Evolve Agent (enhanced)
=============================

Workflow: [Brainstorming Phase] → [Audit Rounds (D1-D7)] → [Report]

[Brainstorming Phase] (NEW - SE-07)
  │
  └── superpowers:brainstorming skill ──► improvement ideas

[Audit Round] (per round, default 5 rounds)
  │
  ├── D1: Paths ── plugin-dev:plugin-structure skill
  │
  ├── D2+D3: Syntax + Frontmatter ──► spawn plugin-validator sub-agent ──► validation report
  │       └── D3 also: plugin-dev:skill-development skill (quality review)
  │
  ├── D4: Cross-ref ── plugin-dev:agent-development skill
  │       └── also: installed_plugins.json cross-reference + collision detection
  │
  ├── D5: Anti-patterns (generic — unchanged)
  │
  ├── D6: Integration ── plugin-dev:plugin-settings skill
  │
  └── D7: Hooks ── plugin-dev:hook-development skill
                          │
                    superpowers:systematic-debugging (applied when issues found)

[Fallback Path] (if plugin-dev not installed)
  │
  └── D2/D3/D4/D6/D7 revert to existing manual checks
```

### Recommended File Changes

```
Changes confined to:
.claude/agents/self-evolve.md     # Frontmatter + body (D2/D3/D4/D6/D7 sections)
```

No other files are modified in this phase.

### Pattern 1: Sub-Agent Spawn for Validation (D2+D3)

**What:** Self-evolve spawns `plugin-dev:plugin-validator` once per audit cycle, passing the TGOSKits plugin root path. The sub-agent runs in its own context window, returns a structured validation report.

**When to use:** For D2 (Syntax) and D3 (Frontmatter) checks. Both fit the plugin-validator's scope: directory structure, manifest JSON, command/agent/skill files, hooks, MCP, file organization.

**Sub-agent spawn invocation (body text in self-evolve.md):**

```
Spawn the `plugin-dev:plugin-validator` agent:

> "Validate the TGOSKits plugin at project root. Check plugin.json manifest,
> all agent files, command files, hooks configuration, skill directories,
> scripts, and file organization. Report all critical issues, warnings,
> and recommendations."
```

**Receiving results:** The plugin-validator outputs a structured report. Self-evolve reads this report and incorporates findings into its D2/D3 classification, then continues with D4-D7.

**Source:** [VERIFIED: plugin-validator.md — agent definition with tools: Read, Grep, Glob, Bash; 10-step validation process; structured report output format]

### Pattern 2: installed_plugins.json Cross-Reference (D4)

**What:** Parse `~/.claude/plugins/installed_plugins.json` to cross-reference every `plugin:skill` reference across all 6 agent files.

**Source:** [VERIFIED: validate-deps.py parsing pattern + installed_plugins.json]

```python
import json, os, glob

PLUGINS_PATH = os.path.expanduser("~/.claude/plugins/installed_plugins.json")
AGENTS_DIR = ".claude/agents"

def get_installed_plugins():
    """Return set of 'plugin:skill' strings from installed_plugins.json."""
    with open(PLUGINS_PATH) as f:
        data = json.load(f)
    installed = set()
    for plugin_key in data.get("plugins", {}):
        # plugin_key format: "name@marketplace"
        plugin_name = plugin_key.split("@")[0]
        # Scan the plugin's skills/
        for entry in data["plugins"][plugin_key]:
            skills_dir = os.path.join(entry["installPath"], "skills")
            if os.path.isdir(skills_dir):
                for skill_dir in os.listdir(skills_dir):
                    if os.path.isfile(os.path.join(skills_dir, skill_dir, "SKILL.md")):
                        installed.add(f"{plugin_name}:{skill_dir}")
    return installed

def scan_agent_skill_refs():
    """Extract all plugin:skill references from all agent files."""
    refs = set()
    for fpath in glob.glob(os.path.join(AGENTS_DIR, "*.md")):
        with open(fpath) as f:
            content = f.read()
        for line in content.splitlines():
            if "plugin:" in line or "superpowers:" in line:
                # Extract skill references from frontmatter and body
                import re
                for match in re.finditer(r'`([a-z][\w-]+:[a-z][\w-]+)`', content):
                    refs.add(match.group(1))
    return refs
```

### Pattern 3: Agent-Name Collision Detection (SE-06)

**What:** Scan all globally installed plugins' agent directories and compare names against TGOSKits agent names.

**Scope decision:** Scan ALL globally installed plugins, not just known-risk namespaces. The scan cost is negligible (<30 agent files) and full coverage catches unexpected collisions.

**Source:** [VERIFIED: installed_plugins.json + plugin cache directory listing]

```bash
# Find all globally installed agent names
for plugin_key in $(python3 -c "import json; d=json.load(open('$HOME/.claude/plugins/installed_plugins.json')); print('\n'.join(d['plugins'].keys()))"); do
  plugin_name=$(echo "$plugin_key" | cut -d@ -f1)
  install_path=$(python3 -c "import json; d=json.load(open('$HOME/.claude/plugins/installed_plugins.json')); print(d['plugins']['$plugin_key'][0]['installPath'])")
  agents_dir="$install_path/agents"
  if [ -d "$agents_dir" ]; then
    for agent_file in "$agents_dir"/*.md; do
      agent_name=$(basename "$agent_file" .md)
      echo "$plugin_name:$agent_name"
    done
  fi
done
```

**Current collision scan result:** ZERO collisions found.

_All_ 6 TGOSKits agent names are unique vs globally installed plugins:
- `pr-review` — not in any global plugin
- `test-gen` — not in any global plugin
- `bug-hunt` — not in any global plugin
- `driver-audit` — not in any global plugin
- `impl` — not in any global plugin
- `self-evolve` — not in any global plugin

**Source:** [VERIFIED: scanned all agents/ dirs in plugin cache: pr-review-toolkit (6 agents), plugin-dev (3), feature-dev (3), hookify (1), agent-sdk-dev (2), code-simplifier (1), superpowers 5.0.7 (1)]

### Pattern 4: Graceful Fallback (D-05)

**What:** If plugin-dev is not installed, D2/D3 revert to manual checks, D4 reverts to existing cross-reference, D6/D7 revert to current manual inspection. A warning is emitted listing which dimensions lost automation.

**When to use:** In the Dependency Check preamble and at the start of each affected dimension.

```
**Preamble check:**
- `plugin-dev:plugin-structure` — (fallback: manual path check)
- `plugin-dev:skill-development` — (fallback: manual frontmatter check)
- `plugin-dev:agent-development` — (fallback: manual cross-ref)
- `plugin-dev:plugin-settings` — (fallback: manual config check)
- `plugin-dev:hook-development` — (fallback: manual hook check)

If all are missing:
> "WARNING: plugin-dev not installed. D2/D3/D4/D6/D7 running in fallback mode (manual checks). Install with: claude plugins install plugin-dev"
```

### Anti-Patterns to Avoid

- **Spawning plugin-validator per-file:** D-06 explicitly says once per cycle. Per-file spawns would consume excessive context and time.
- **Hardcoding installed_plugins.json path without expansion:** Always use `os.path.expanduser()` — D4 must work across environments.
- **Making collision detection a hard block:** D-04 says warning-only. A hard block on non-critical namespace conflicts would annoy users.
- **Loading ALL plugin-dev skills at once:** D-03 says selective loading by dimension keeps context lean.
- **Modifying plugin-dev files:** Out of scope per REQUIREMENTS.md. The plugin is consumed, not forked.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Plugin structure/syntax validation | Custom JSON/YAML/Bash parsers | `plugin-dev:plugin-validator` agent | Validates 10 dimensions (manifest, agents, commands, skills, hooks, MCP, file org, security) — far more than self-evolve's current D2+D3 |
| Skill quality review | Manual checklist | `plugin-dev:skill-development` skill | Covers writing style, progressive disclosure, trigger phrases, validation — comprehensive methodology |
| Agent quality review | Manual frontmatter checks | `plugin-dev:agent-development` skill | Covers identifier validation, description quality, system prompt design, triggering conditions |
| Hook structure validation | Manual hooks.json inspection | `plugin-dev:hook-development` skill | Covers hook events, configuration formats, matchers, security, performance — includes validation scripts |
| Settings/config consistency | Manual settings.json inspection | `plugin-dev:plugin-settings` skill | Covers `.local.md` pattern, YAML frontmatter parsing, gitignore, defaults, security |

**Key insight:** The plugin-dev skills exist specifically to automate the same quality audits self-evolve currently performs manually. Each skill provides a structured methodology with reference implementations. The plugin-validator agent alone handles what currently takes ~40 lines of D2+D3 bash commands.

## Common Pitfalls

### Pitfall 1: Sub-Agent Spawn Overhead
**What goes wrong:** plugin-validator spawn consumes a new context window, adding latency to each audit cycle. If spawned per-file, cost multiplies.
**Why it happens:** Each agent spawn initializes a new context.
**How to avoid:** Spawn ONCE per cycle (D-06), passing all files for validation. The plugin-validator handles multiple files in a single spawn.
**Warning signs:** Audit cycles taking >2x expected time.

### Pitfall 2: Frontmatter Skills Overload
**What goes wrong:** Loading 7 new skills into self-evolve's frontmatter increases context load and may trigger unintended auto-activation during auditing.
**Why it happens:** Skills in frontmatter are auto-discovered and may trigger on description matching unrelated to the audit dimension.
**How to avoid:** Keep frontmatter `description` field focused on self-evolve's purpose, not listing individual skills. Skills are invoked by name in the audit body, not by description matching.
**Warning signs:** Self-evolve outputs unrelated skill behavior during an audit round.

### Pitfall 3: installed_plugins.json Path Resolution
**What goes wrong:** D4 cross-reference fails because it can't find installed_plugins.json or uses wrong path.
**Why it happens:** The path `~/.claude/plugins/installed_plugins.json` uses shell tilde, which Python's `open()` does not expand without `os.path.expanduser()`.
**How to avoid:** Use `os.path.expanduser("~/.claude/plugins/installed_plugins.json")` — same pattern as validate-deps.py.
**Warning signs:** D4 reports "file not found" error.

### Pitfall 4: Cyclic Audit Loop Breakage
**What goes wrong:** Adding sub-agent spawn + skill loading breaks self-evolve's existing D1-D7 round structure.
**Why it happens:** The round workflow is tightly coupled to sequential dimension execution. A mis-ordered spawn could skip dimensions.
**How to avoid:** Keep the D1-D7 loop structure intact. The sub-agent spawn REPLACES inline D2/D3 code. Skills are loaded upfront (frontmatter) and invoked inline in their dimension section. Do not restructure the loop.
**Warning signs:** Audit runs report D2 but not D3, or skip dimensions after D2 spawn.

### Pitfall 5: Collision Detection False Positives
**What goes wrong:** The grep-based scan matches partial name overlaps (e.g., "review" matching both "pr-review" and "code-reviewer").
**Why it happens:** Simple grep doesn't check for exact basename match.
**How to avoid:** Match basenames exactly: extract `basename "$agent_file" .md` from the agents/ directory, and compare against TGOSKits agent names using exact string comparison.
**Warning signs:** Collision warnings for unrelated agents.

## Code Examples

### Example 1: plugin-validator sub-agent spawn in self-evolve body

```markdown
### D2 + D3: Automated Syntax and Frontmatter Validation (via plugin-validator)

Spawn the `plugin-dev:plugin-validator` agent:

> "Validate the TGOSKits plugin at the project root ($PWD/.claude/).
> Check the following and report all critical issues, warnings, and recommendations:
> 1. plugin.json manifest correctness (JSON syntax, required fields, name format)
> 2. All agent files (agents/*.md): YAML frontmatter, name/description/model/color/tools fields
> 3. All command files (commands/*.md): YAML frontmatter, description, arg-hint if present
> 4. hooks/hooks.json: JSON syntax, valid event names, proper hook structure
> 5. Skill directories: SKILL.md existence, frontmatter presence
> 6. File organization: README exists, no unnecessary files
> 7. Naming conventions: kebab-case, no duplicates"

After the sub-agent returns, incorporate findings into the audit classification.
Classify results as BLOCK/WARN/INFO per dimension D2 and D3.
```

**Source:** [VERIFIED: plugin-validator.md — agent definition and 10-step validation process]

### Example 2: Skill-on-Demand Invocation Pattern

```markdown
### D7: Hook Integration

Check all agent actions against registered hooks:

1. Invoke `plugin-dev:hook-development`:
   - Review hooks/hooks.json structure and completeness
   - Check hook scripts for security patterns (path traversal, input validation)
   - Verify ${CLAUDE_PLUGIN_ROOT} usage
   - Validate hook events are properly matched
```

### Example 3: Collision Detection Implementation

```python
import json, os, glob

def check_agent_name_collisions(agents_dir=".claude/agents"):
    """Check TGOSKits agent names against globally installed plugin agents.
    Returns list of (plugin_name, agent_name) collisions or empty list."""
    tgoskits_agents = set()
    for fpath in glob.glob(os.path.join(agents_dir, "*.md")):
        name = os.path.basename(fpath).replace(".md", "")
        tgoskits_agents.add(name)

    plugins_path = os.path.expanduser("~/.claude/plugins/installed_plugins.json")
    with open(plugins_path) as f:
        data = json.load(f)

    collisions = []
    for plugin_key, entries in data.get("plugins", {}).items():
        plugin_name = plugin_key.split("@")[0]
        for entry in entries:
            install_path = entry["installPath"]
            agents_path = os.path.join(install_path, "agents")
            if os.path.isdir(agents_path):
                for agent_file in glob.glob(os.path.join(agents_path, "*.md")):
                    global_name = os.path.basename(agent_file).replace(".md", "")
                    if global_name in tgoskits_agents:
                        collisions.append((plugin_name, global_name))
    return collisions
```

### Example 4: Brace Expansion for `installed_plugins.json` Parsing

```python
# Reuse validate-deps.py's parsing directly by importing or adapting
from validate_deps import parse_version, check_plugins

# For D4, we need different logic than check_plugins():
# - List ALL installed plugins (not just required ones)
# - For each, list available skills
# - Cross-reference against agent file references

def list_available_skills(plugins_path=None):
    """Return dict of {plugin_name: [skill_names]} from installed_plugins.json."""
    if plugins_path is None:
        plugins_path = os.path.expanduser("~/.claude/plugins/installed_plugins.json")
    with open(plugins_path) as f:
        data = json.load(f)

    available = {}
    for plugin_key, entries in data.get("plugins", {}).items():
        plugin_name = plugin_key.split("@")[0]
        skills = []
        for entry in entries:
            skills_dir = os.path.join(entry["installPath"], "skills")
            if os.path.isdir(skills_dir):
                for sd in os.listdir(skills_dir):
                    if os.path.isfile(os.path.join(skills_dir, sd, "SKILL.md")):
                        skills.append(f"{plugin_name}:{sd}")
        if skills:
            available[plugin_name] = skills
    return available
```

**Source:** [VERIFIED: validate-deps.py — `check_plugins()` function pattern, installed_plugins.json format]

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| D2/D3: Manual `python3 -m json.tool` + `sed` parsing | Automated plugin-validator sub-agent spawn | This phase | Comprehensive validation, own context window, structured report |
| D4: Manual cross-reference (self-evolve only) | Cross-reference ALL agent files + installed_plugins.json + collision detection | This phase | Catches stale references across all agents, not just self-evolve |
| D6: Manual inspection | plugin-dev:plugin-settings skill | This phase | Structured settings/config methodology |
| D7: Manual hook inspection | plugin-dev:hook-development skill | This phase | Hook structure validation, security patterns |
| No improvement ideation | superpowers:brainstorming phase before audit rounds | This phase | Structured design process before auditing |
| No root-cause analysis | superpowers:systematic-debugging for quality issues | This phase | Iron Law: no fixes without root cause investigation |

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `plugin-dev:plugin-settings` is the correct skill for D6 (not `mcp-integration`) | Standard Stack | D6 would load an irrelevant skill (mcp-integration) about external MCP servers when TGOSKits has none |
| A2 | All 5 plugin-dev skills listed by CONTEXT.md are available in the installed version | Standard Stack | Skills might be renamed or restructured in future plugin-dev versions |
| A3 | Collision detection scanning all plugins (not just known-risk) is acceptable performance | Don't Hand-Roll | Scan covers <30 agent files; even on slow filesystems this is <1 second |

## Assumptions Verification Notes

- **A1**: TGOSKits has empty `settings.json` (`{}`), no `.mcp.json`, no MCP servers in `plugin.json`. `plugin-dev:plugin-settings` covers `.local.md` pattern, YAML parsing, gitignore, defaults — all relevant for settings consistency audit. `mcp-integration` covers external server configuration — irrelevant. [VERIFIED: project files]
- **A2**: All 5 skills confirmed present in `plugin-dev/1a2f18b05cf5/skills/`: plugin-structure, skill-development, agent-development, plugin-settings, hook-development. [VERIFIED: directory listing]
- **A3**: Full scan completed: 26 agent files across ~10 plugins. Performance impact negligible. [VERIFIED: directory listing]

## Open Questions (RESOLVED)

1. **How should self-evolve handle the plugin-validator's output format?**
   - Recommendation: Direct mapping (critical=BLOCK, major=WARN, minor=INFO) is simplest and works because plugin-validator already uses severity levels aligned with self-evolve's.
   - **RESOLVED:** Direct mapping selected. Plans implement this in 02-02 T1.

2. **When in the round does the brainstorming phase run?**
   - Recommendation: Run once before round 1. Self-evolve is a cyclic audit tool — brainstorming improvement ideas should happen before the first audit, not between rounds.
   - **RESOLVED:** Run once before round 1. Plans implement this in 02-03 T2.

3. **Should existing D5 (anti-patterns) be updated too?**
   - Recommendation: D5 stays unchanged. Plugin-dev skills may reveal new anti-patterns, but those will be surfaced through the other dimensions' findings.
   - **RESOLVED:** D5 unchanged. No plan task touches D5 beyond preservation check.

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| plugin-dev plugin | D2/D3/D4/D6/D7 | Yes (optional) | 1a2f18b05cf5 | Graceful fallback to manual checks (D-05) |
| superpowers plugin | SE-07, SE-08 | Yes (hard dep) | 5.1.0 | Already verified by Phase 1 session hook |

**Missing dependencies with no fallback:**
- None — plugin-dev is optional; superpowers is already a hard dependency verified by Phase 1.

**Missing dependencies with fallback:**
- plugin-dev: All 5 dimensions fall back to existing manual checks with a clear warning.

## Validation Architecture

### Test Framework

This phase modifies Claude Code agent markdown files, not code. Traditional unit tests do not apply.

| Property | Value |
|----------|-------|
| Framework | Manual verification (no automated test framework for agent files) |
| Config file | None |
| Quick run command | `cat .claude/agents/self-evolve.md` (verify structure) |
| Full suite command | Manual: invoke `/self-evolve 1` and verify D2/D3/D4/D6/D7 run correctly |

### Phase Requirements -> Test Map

| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| SE-01 | plugin-validator spawns for D2+D3 | Manual | Invoke self-evolve, check output for "Plugin Validation Report" | N/A |
| SE-02 | D4 scans installed_plugins.json | Manual | Invoke self-evolve, check D4 cross-ref finds installed skills | N/A |
| SE-03 | skill-development skill loaded | Manual | Verify skill listed in frontmatter `skills:` | N/A |
| SE-04 | agent-development skill loaded | Manual | Verify skill listed in frontmatter | N/A |
| SE-05 | hook-development skill loaded | Manual | Verify skill listed in frontmatter | N/A |
| SE-06 | Collision detection warns | Manual | Invoke self-evolve, check D4 report for collision section | N/A |
| SE-07 | brainstorming skill loaded | Manual | Verify skill listed in frontmatter | N/A |
| SE-08 | systematic-debugging skill loaded | Manual | Verify skill listed in frontmatter | N/A |

### Sampling Rate
- **Per task commit:** Manual review of self-evolve.md frontmatter and body
- **Per wave merge:** Full D1-D7 run with `self-evolve 1` to verify no regression
- **Phase gate:** `/self-evolve 1` completes successfully with automated D2/D3/D4/D6/D7

### Wave 0 Gaps
- [ ] No automated test infrastructure exists for agent markdown files — Phase 2 does not introduce code changes, so this is acceptable. All validation is manual.

## Security Domain

### Applicable ASVS Categories

This phase does not add authentication, session management, access control, or cryptography. The changes are to agent behavior and skill loading.

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | No | N/A |
| V3 Session Management | No | N/A |
| V4 Access Control | No | N/A |
| V5 Input Validation | No | N/A |
| V6 Cryptography | No | N/A |

### Known Threat Patterns

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Sub-agent spawns untrusted content | Spoofing | plugin-validator is from plugin-dev (official plugin, installed via claude plugins) |
| Collision detection reading plugin files | Tampering | Read-only; no file modification |
| installed_plugins.json parsing | Tampering | JSON parsing with try/except per validate-deps.py pattern |

## Sources

### Primary (HIGH confidence)

- [VERIFIED: installed_plugins.json] — Format: version 2, plugin keys as `name@marketplace`, entries with scope/installPath/version/installedAt/lastUpdated
- [VERIFIED: plugin-dev agent files] — Agent: plugin-validator (10-step validation, structured report output). All 3 agents: agent-creator, plugin-validator, skill-reviewer
- [VERIFIED: plugin-dev skill files] — 7 skills confirmed: plugin-structure, skill-development, agent-development, hook-development, plugin-settings, command-development, mcp-integration
- [VERIFIED: self-evolve.md] — 250 lines, 7 audit dimensions (D1-D7), cyclic round workflow, `superpowers:verification-before-completion` skill in frontmatter
- [VERIFIED: validate-deps.py] — Python pattern for installed_plugins.json parsing with `parse_version()` and `check_plugins()`
- [VERIFIED: superpowers 5.1.0 skills] — 14 skills including brainstorming and systematic-debugging
- [VERIFIED: global agent collision scan] — Zero collisions between 6 TGOSKits agents and ~20 global plugin agents across ~10 plugins

### Secondary (MEDIUM confidence)

- [VERIFIED: CONTEXT.md decision mapping] — D-01 through D-06, skill loading table, Claude's Discretion items all confirmed consistent with actual plugin-dev capabilities

### Tertiary (LOW confidence)

None — all major claims have been verified against actual files.

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — all skills and agents verified against actual plugin-dev files and installed_plugins.json
- Architecture: HIGH — sub-agent spawn, skill loading, collision detection, fallback patterns all verified against existing implementations (impl.md spawn pattern, validate-deps.py parsing, plugin-validator agent definition)
- Pitfalls: MEDIUM — based on general agent integration patterns; actual behavior depends on runtime environment

**Research date:** 2026-05-14
**Valid until:** 2026-06-14 (plugin-dev and superpowers are stable plugins)
