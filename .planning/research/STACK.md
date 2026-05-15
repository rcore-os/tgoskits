# Plugin Composition Stack

## Research Summary

Investigation of how a project-local Claude Code plugin (TGOSKits at `.claude/`) leverages agents and skills from installed plugins (superpowers@5.1.0, pr-review-toolkit, code-review, plugin-dev).

**Date**: 2026-05-13
**Scope**: Runtime resolution of `skills:`, `tools:`, agent delegation, and agent type discovery across plugin boundaries.

---

## Skill Resolution

### How the `skills:` frontmatter field works across plugins

Claude Code builds a unified skill registry at session start by scanning all installed plugins. The resolution path is:

```
Session start
  -> Scan user plugins (~/.claude/plugins/cache/*/)
  -> Scan project plugins (.claude/)
  -> Build unified registry: { "plugin:skill" -> path/to/SKILL.md }
  -> Merge with system-reminder "Available skills" list
```

**Evidence**: The system-reminder in this session lists both project-local skills (`starry-test-suit`, `cross-kernel-driver`) and plugin skills (`superpowers:systematic-debugging`, `superpowers:verification-before-completion`) as available. This merged list is the runtime registry.

### Namespace convention

| Reference form | Resolution | Example |
|---------------|-----------|---------|
| `skill-name` | Project-local skill (`.claude/skills/<name>/SKILL.md`) | `starry-test-suit` |
| `plugin:skill-name` | Installed plugin skill | `superpowers:systematic-debugging` |

The colon-separated prefix maps to the plugin's `name` field in `plugin.json` or `.claude-plugin/plugin.json`. The runtime strips the prefix to locate `SKILL.md` within the plugin's `skills/` directory.

**Confirmed in TGOSKits agents:**

```yaml
# agents/bug-hunt.md (line 4-9)
skills:
  - starry-test-suit          # project-local
  - cross-kernel-driver        # project-local
  - arceos-test-adapter        # project-local
  - superpowers:systematic-debugging       # cross-plugin
  - superpowers:verification-before-completion  # cross-plugin
```

```yaml
# agents/self-evolve.md (line 4-5)
skills:
  - superpowers:verification-before-completion  # cross-plugin only
```

### Skill invocation mechanics

The `Skill` tool is the activation mechanism. When an agent body says "invoke `superpowers:systematic-debugging`", it calls the `Skill` tool with `skill: "superpowers:systematic-debugging"`. The runtime loads SKILL.md content from the corresponding plugin's `skills/` directory.

Per `using-superpowers/SKILL.md`:
> "In Claude Code: Use the Skill tool. When you invoke a skill, its content is loaded and presented to you—follow it directly. Never use the Read tool on skill files."

### Skill auto-discovery scope

Skills are discovered from:
1. **User-installed plugins**: `~/.claude/plugins/cache/<marketplace>/<plugin-name>/<version>/skills/*/SKILL.md`
2. **Project-local skills**: `.claude/skills/*/SKILL.md`
3. **Marketplace source plugins**: `~/.claude/plugins/marketplaces/<marketplace>/plugins/<name>/skills/` (if applicable)

Superpowers registers 14 skills across `systematic-debugging/`, `verification-before-completion/`, `brainstorming/`, `test-driven-development/`, etc. Each has `name: <skill-name>` in its SKILL.md frontmatter.

---

## Agent Delegation

### How the Agent tool discovers agents from installed plugins

Agent discovery follows the same merged-registry pattern as skills. At session start, Claude Code builds a unified agent namespace from:

1. **User-installed plugin agents**: `<plugin-cache>/agents/*.md`
2. **Project-local agents**: `.claude/agents/*.md`
3. **Marketplace source agents**: `~/.claude/plugins/marketplaces/.../agents/*.md`

Each agent's frontmatter `name:` field becomes the agent identifier. The plugin namespace prefix (`plugin-name:`) disambiguates agents with the same name across plugins.

### Agent namespace resolution

| Reference form | Resolution | Example |
|---------------|-----------|---------|
| `agent-name` | Project-local agent or globally unique agent | `test-gen`, `pr-review` (TGOSKits) |
| `plugin:agent-name` | Agent from installed plugin | `pr-review-toolkit:code-reviewer` |
| `debugger`, `security-auditor` | Referenced by name in TGOSKits agents | Must exist in merged namespace |

**Evidence from TGOSKits agents:**

```markdown
<!-- bug-hunt.md line 27 -->
For complex debugging scenarios, spawn the `debugger` agent...
For security-sensitive bugs, spawn the `security-auditor` agent...
```

```markdown
<!-- impl.md line 27 -->
For test generation (Phase 3), spawn the `test-gen` agent...
For code touching memory safety, spawn the `security-auditor` agent...
```

```markdown
<!-- impl.md line 350 -->
Spawn the `pr-review` agent on the current branch...
```

### Cross-plugin agent spawning format

When TGOSKits needs an agent from another plugin, it uses the `Agent` tool (not the `Task` tool) with the qualified name. For example, `pr-review-toolkit:code-reviewer` would spawn the code-reviewer agent from pr-review-toolkit.

The `pr-review-toolkit:review-pr` command demonstrates this pattern:
```markdown
<!-- review-pr.md lines 45-57 -->
Launch Review Agents:
  - Sequential approach (one at a time)
  - Parallel approach (user can request)
Available agents: comment-analyzer, pr-test-analyzer, silent-failure-hunter,
                   type-design-analyzer, code-reviewer, code-simplifier
```

These agents are referenced within the same plugin namespace, but the same mechanism works across plugins.

### Agent frontmatter differences: project-local vs plugin

Project-local agents (TGOSKits):
```yaml
---
name: bug-hunt
description: Find bugs...
skills:
  - superpowers:systematic-debugging
tools:
  - Read
  - Write
  - Edit
  - Bash
  - Grep
  - Glob
---
```

Plugin agents (pr-review-toolkit):
```yaml
---
name: code-reviewer
description: Use this agent when...
model: opus       # model selection
color: green      # UI color
---
```

**Key difference**: Plugin agents support `model:`, `color:`, `disable-model-invocation:`, and `allowed-tools:` fields. Project-local agents use `skills:` and `tools:`. Both forms are valid and the runtime handles them.

### The `tools:` field

The `tools:` field in project-local agents restricts which Claude Code tools the agent has access to. TGOSKits agents use: `Read`, `Write`, `Edit`, `Bash`, `Grep`, `Glob`, `WebSearch`, `WebFetch`. This is a security boundary — agents cannot access tools not listed.

Plugin agents use `tools:` (note: plural without `s` in some) or `allowed-tools:` with more granular control (e.g., `Bash(gh pr view:*)`).

---

## Compatibility Model

### Version tracking mechanism

Installed plugins are tracked in `~/.claude/plugins/installed_plugins.json`:

```json
{
  "version": 2,
  "plugins": {
    "superpowers@claude-plugins-official": [{
      "scope": "user",
      "installPath": ".../superpowers/5.1.0",
      "version": "5.1.0",
      "gitCommitSha": "917e5f53b16b115b70a3a355ed5f4993b9f8b73d"
    }],
    "pr-review-toolkit@claude-plugins-official": [{
      "scope": "user",
      "installPath": ".../pr-review-toolkit/unknown",
      "version": "unknown"
    }]
  }
}
```

### What happens when plugins update

1. **Skill content changes**: If a plugin updates and a skill's content changes, the new content takes effect on next session start. There is **no lockfile** preventing this — the runtime always uses the currently installed version.

2. **Skill names change**: If a skill is renamed or removed, any `skills:` reference to `plugin:old-skill-name` becomes a **silent failure** — the skill simply doesn't appear in the available list. The agent will not receive the skill content at runtime. **No error is raised**.

3. **Agent names change**: Similar to skills — references to removed/renamed agents fail silently.

4. **Multiple cached versions**: The cache directory retains old versions (e.g., superpowers 5.0.7 and 5.1.0 coexist). The active version is determined by `installed_plugins.json`. Old versions are marked with `.orphaned_at` but not immediately deleted.

### Risk categories

| Risk | Severity | Mitigation |
|------|----------|-----------|
| Skill renamed/removed in plugin update | **HIGH** | TGOSKits agents silently lose capability. No warning at session start. |
| Skill content changes (behavior drift) | **MEDIUM** | Agent behavior may subtly change. Core principles usually stable in mature plugins. |
| Agent renamed/removed in plugin update | **LOW** | TGOSKits agents reference their own agents (not external), so this mainly affects pr-review-toolkit references. |
| Plugin uninstalled | **HIGH** | All `skills:` references to that plugin become dead. Agents referencing its skills would fail to load them. |

### Superpowers version stability

Superpowers is at 5.1.0 (pinned by git SHA `917e5f53`). The skill names are stable across versions observed (5.0.7 and 5.1.0 share the same skill names). However, skill content has changed between versions — the CHANGELOG.md likely documents these changes.

### Dependency declaration gap

**There is no mechanism for Plugin A to declare a dependency on Plugin B.** TGOSKits' `plugin.json` has no `dependencies:` or `requires:` field. If superpowers were uninstalled, TGOSKits agents would silently lose their cross-plugin skills with no warning.

---

## Recommendations

### R1: Pin plugin versions explicitly (HIGH)

**What**: Document the exact plugin versions TGOSKits agents expect in a project-level dependency manifest.

**Why**: Silent skill-resolution failures are the hardest to debug. If a user upgrades superpowers to 6.0.0 and `systematic-debugging` changes name, the bug-hunt agent silently loses its debugging workflow with no error.

**How**: Add a `.claude/plugin-dependencies.md` or inline comment in `plugin.json` documenting minimum versions:

```json
{
  "name": "tgoskits",
  "version": "0.1.0",
  "_plugin_dependencies": {
    "superpowers": ">=5.1.0",
    "pr-review-toolkit": "*"
  }
}
```

Also add a D4 (Cross-Reference Consistency) check to the self-evolve agent that verifies all `plugin:skill` references resolve to installed plugins at audit time.

### R2: Use `superpowers:` prefix consistently (HIGH)

**What**: All TGOSKits agents that reference superpowers skills do so with the `superpowers:` prefix. Maintain this convention.

**Why**: The prefix is what the runtime uses to resolve the skill location. Without it, the skill would be looked up as a project-local skill and fail to resolve.

**Status**: Already done. All 5 TGOSKits agents consistently use `superpowers:systematic-debugging` and `superpowers:verification-before-completion` in their frontmatter.

### R3: Audit unqualified agent references (MEDIUM)

**What**: The bug-hunt agent references `debugger` and `security-auditor` agents without namespace prefixes. Verify these exist in the merged namespace.

**Why**: These agents don't exist in TGOSKits (`plugin.json` agents list: `pr-review`, `test-gen`, `bug-hunt`, `driver-audit`, `impl`, `self-evolve`). They may be user-global agents from `.claude/agents/` or agents from other installed plugins. If they don't exist, these are dead references.

**How**: Run a cross-reference audit (already part of self-evolve D4):
```bash
# Check if referenced agents exist
for agent in debugger security-auditor test-gen pr-review; do
  found=$(find .claude/agents ~/.claude/plugins -name "${agent}.md" 2>/dev/null | head -1)
  [ -z "$found" ] && echo "MISSING AGENT: $agent"
done
```

### R4: Add self-evolve D4 check for cross-plugin skill validity (MEDIUM)

**What**: Extend the self-evolve agent's D4 (Cross-Reference Consistency) to verify that all `plugin:skill` references in frontmatter resolve to installed plugins.

**Why**: The current D4 check mentions global skills but doesn't explicitly validate against `installed_plugins.json`. Adding this ensures early detection of broken references.

**How**: Add to the self-evolve D4 checklist:
```markdown
- Every `plugin:skill` reference in agent frontmatter resolves to an installed plugin
  (verify plugin exists in `~/.claude/plugins/installed_plugins.json` and
   skill file exists under the plugin's install path)
```

### R5: Document the skill resolution contract (MEDIUM)

**What**: Add a `references/plugin-composition.md` file to the `cross-kernel-driver` or a new shared skill explaining how skill/agent resolution works.

**Why**: Future agent authors need to understand the namespace convention (`plugin:skill`) to correctly reference external skills. Without documentation, they'll trial-and-error.

**How**: Include:
- Namespace convention (`plugin:skill` vs bare `skill`)
- How to verify a skill reference resolves
- How to test cross-plugin agent spawning
- What happens when a referenced plugin is uninstalled

### R6: Keep superpowers skill references at the action level, not just frontmatter (LOW - already done)

**What**: Continue the current pattern where agents both list skills in frontmatter AND explicitly describe when to invoke them in the agent body.

**Why**: Frontmatter declares availability; body text defines when to use. Both are needed for reliable behavior. The `using-superpowers` skill emphasizes this: "If you think there is even a 1% chance a skill might apply, you ABSOLUTELY MUST invoke the skill."

**Status**: Already done. Example from bug-hunt.md:
```markdown
## Global Capabilities
Before implementing fixes, invoke `superpowers:systematic-debugging`...
Before claiming any fix is complete, invoke `superpowers:verification-before-completion`...
```

### R7: Consider model specification for cross-plugin agent spawning (LOW)

**What**: When TGOSKits agents spawn agents from other plugins (e.g., `pr-review-toolkit:code-reviewer`), note that the spawned agent may use a different model (`model: opus` in pr-review-toolkit agents).

**Why**: The TGOSKits project-local agents don't specify a model, so they use the session default. Plugin agents may override this. For cost-sensitive workflows, understanding model routing is important.

**How**: Document in agent body when spawning an agent that uses a specific model. This is informational — the runtime handles model selection automatically.

---

## Architecture Diagram

```
Session Start
    |
    v
+---------------------------+
| Plugin Discovery Engine   |
| (Claude Code Runtime)     |
+---------------------------+
    |
    |-- Scans ~/.claude/plugins/cache/*/
    |-- Scans .claude/ (project-local)
    |-- Reads installed_plugins.json
    |
    v
+---------------------------+
| Unified Namespace Registry |
+---------------------------+
    |
    |-- Skills: { "plugin:skill" -> path }
    |-- Agents: { "plugin:agent" -> path }
    |-- Commands: { "plugin:cmd" -> path }
    |-- Hooks: { event -> [scripts] }
    |
    v
+---------------------------+
| System Reminder Injection  |
| ("Available skills: ...")  |
+---------------------------+
    |
    v
+---------------------------+
| Agent/Skill Resolution     |
| at invocation time         |
+---------------------------+
    |
    |-- Skill tool: loads SKILL.md content
    |-- Agent tool: spawns subagent with its own context
    |-- Command: runs slash command
```

## Plugin Install Paths (current state)

| Plugin | Version | Install Path |
|--------|---------|-------------|
| superpowers | 5.1.0 (git: 917e5f53) | `~/.claude/plugins/cache/claude-plugins-official/superpowers/5.1.0/` |
| pr-review-toolkit | unknown | `~/.claude/plugins/cache/claude-plugins-official/pr-review-toolkit/unknown/` |
| code-review | unknown | `~/.claude/plugins/cache/claude-plugins-official/code-review/unknown/` |
| plugin-dev | unknown | `~/.claude/plugins/cache/claude-plugins-official/plugin-dev/unknown/` |
| TGOSKits (project) | 0.1.0 | `./.claude/` |

## TGOSKits Agent Skill Map

| Agent | Project-local skills | Superpowers skills |
|-------|---------------------|-------------------|
| bug-hunt | starry-test-suit, cross-kernel-driver, arceos-test-adapter | systematic-debugging, verification-before-completion |
| pr-review | review-open-prs, starry-test-suit, arceos-test-adapter | verification-before-completion |
| impl | starry-test-suit, arceos-test-adapter | verification-before-completion |
| self-evolve | (none) | verification-before-completion |
| driver-audit | cross-kernel-driver | verification-before-completion |
| test-gen | starry-test-suit, arceos-test-adapter | verification-before-completion |

All 6 TGOSKits agents depend on the `superpowers:verification-before-completion` skill. This makes superpowers a **hard runtime dependency** of the TGOSKits plugin.
