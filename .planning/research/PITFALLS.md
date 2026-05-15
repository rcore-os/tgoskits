# Plugin Integration Pitfalls -- PITFALLS Dimension

> Research date: 2026-05-13
> Scope: What goes wrong when Claude Code plugins try to use other plugins, and what TGOSKits should avoid.

---

## Current TGOSKits Plugin Landscape

### Local Plugin (`.claude/plugin.json`, version 0.1.0)
- **6 agents**: `pr-review`, `test-gen`, `bug-hunt`, `driver-audit`, `impl`, `self-evolve`
- **4 commands**: `test`, `pr-prep`, `impl`, `self-evolve`
- **6 local skills**: `arceos-test-adapter`, `board-uboot-fsck-repair`, `cross-kernel-driver`, `review-open-prs`, `starry-test-suit`, `update-std-tests`
- **4 hooks**: PreToolUse (Bash), PostToolUse (Edit|Write), Stop
- **7 hook scripts**: `docker-check.py`, `pre-pr-gate.py`, `post-tool-use-log.py`, `stop-hook.py`, `syscall-diff.py`, `journal-generator.py`, `local-ci.sh`

### Installed Global Plugins (19 plugins from `claude-plugins-official`)
`superpowers@5.1.0`, `context7`, `github`, `code-review`, `feature-dev`, `hookify`, `ralph-loop`, `skill-creator`, `commit-commands`, `playwright`, `chrome-devtools-mcp`, `frontend-design`, `claude-md-management`, `plugin-dev`, `serena`, `pr-review-toolkit`, `code-simplifier`, `agent-sdk-dev`, `huggingface-skills`, `playground`, `claude-code-setup`

### Marketplace Sources (3 registries)
`claude-plugins-official`, `karpathy-skills` (`andrej-karpathy-skills`), `anthropic-agent-skills`

---

## Cross-Plugin Reference Map

TGOSKits agents reference entities outside the local plugin:

| TGOSKits Agent | External Skill References | External Agent Spawns | External MCP/Service |
|---|---|---|---|
| `bug-hunt` | `superpowers:systematic-debugging`, `superpowers:verification-before-completion` | `security-auditor`, `debugger` | `context7` MCP |
| `pr-review` | `superpowers:verification-before-completion` | `security-auditor` | `context7` MCP |
| `driver-audit` | `superpowers:verification-before-completion` | `security-auditor` | `context7` MCP |
| `impl` | `superpowers:verification-before-completion` | `test-gen`, `pr-review`, `security-auditor` | `context7` MCP, WebSearch |
| `test-gen` | `superpowers:verification-before-completion` | (none) | `context7` MCP |
| `self-evolve` | `superpowers:verification-before-completion` | (none) | (none) |

---

## PITFALL 1: Cross-Plugin Skill Resolution Breaks Silently

### What goes wrong

Every TGOSKits agent frontmatter references `superpowers:verification-before-completion` and `bug-hunt` also references `superpowers:systematic-debugging`. These are skills from the externally installed `superpowers` plugin.

**Failure scenarios:**

1. **Plugin uninstalled**: If a user runs `claude plugins uninstall superpowers`, all TGOSKits agents that invoke `superpowers:*` skills will produce an error at invocation time. Claude Code may silently skip the missing skill or fail with an obscure error.

2. **Skill renamed**: If superpowers v6.0 renames `verification-before-completion` to `verify-before-done`, every TGOSKits agent frontmatter becomes stale. No error at plugin load time -- only at invocation time.

3. **Skill removed**: If superpowers deprecates `systematic-debugging` in favor of a different debugging approach, `bug-hunt` agent breaks.

4. **Namespace change**: If superpowers changes from `superpowers:` prefix to some other namespace scheme, all references break.

### Warning signs

- Agent invocation shows "skill not found" or "unknown skill" warnings
- Agent proceeds but skips verification steps that were previously required
- Agent behavior degrades gradually (missing verification) rather than failing loudly

### Prevention strategies

1. **Pin superpowers version** in a project-local constraint file (currently version `unknown` or `5.1.0` is tracked but not enforced)
2. **Add a CI check** that validates all cross-plugin skill references resolve
3. **Fallback pattern**: Agents should check "if skill X available, use it; otherwise use manual equivalent"
4. **Version contract**: Document which superpowers version the project agents are tested against

### Which phase should address

`self-evolve` phase (D4 cross-reference consistency check already exists but only verifies names exist -- does NOT verify cross-plugin resolution)

### Testing strategy

```bash
# Simulate missing skill: temporarily rename the skill directory
# Then run each agent to see if it fails gracefully vs silently
for agent in bug-hunt pr-review driver-audit impl test-gen self-evolve; do
  echo "=== Testing agent: $agent with missing superpowers ==="
  # (manual test: invoke agent and check error handling)
done
```

---

## PITFALL 2: Agent/Command Name Collisions

### What goes wrong

TGOSKits has local agents named `pr-review`, `test-gen`, `impl`, `bug-hunt`, `driver-audit`, `self-evolve`. If ANY globally installed plugin introduces an agent or command with the same name, Claude Code must resolve the conflict.

**Actual collision risk**: The `pr-review-toolkit` global plugin has a skill `pr-review-toolkit:review-pr`. While not an exact name match, the similarity creates confusion risk.

**Other collision vectors**:
- `code-review` global plugin has `code-review:code-review` skill -- name is distinct but conceptually similar to `pr-review`
- `impl` is a very common name -- high risk of collision with other plugins
- `test` is used as a command name -- extremely generic, high collision risk

### Warning signs

- "Duplicate command/agent name" error at plugin load time
- The wrong agent executes when invoked by name
- Agent behavior changes unexpectedly because a different plugin's agent takes precedence

### Prevention strategies

1. **Namespace all local agent names** with a prefix: `tg-impl` instead of `impl`, `tg-pr-review` instead of `pr-review`
2. **Check for collisions** in CI before merging: scan all installed plugins for name conflicts
3. **Use fully-qualified references** when spawning agents: `tgoskits:pr-review` instead of just `pr-review`
4. **Document the precedence rules** for local vs global agent resolution

### Which phase should address

`self-evolve` phase (D4 dimension already checks for duplicates within the plugin, but does NOT check against global plugins)

### Testing strategy

```bash
# Collision detection script
python3 -c "
import json, os, subprocess
# Get local agent/command names
local_agents = ['pr-review','test-gen','bug-hunt','driver-audit','impl','self-evolve']
local_cmds = ['test','pr-prep','impl','self-evolve']
# Get global agent/command names from installed plugins
# (parse installed_plugins.json, then each plugin's manifest)
# Report any collisions
"
```

---

## PITFALL 3: Agent Delegation to Non-Existent Agents

### What goes wrong

Multiple TGOSKits agents spawn/delegate to other agents:

| Caller | Spawns | Where Defined |
|---|---|---|
| `bug-hunt` | `security-auditor` | NOT in TGOSKits plugin -- must be global |
| `bug-hunt` | `debugger` | NOT in TGOSKits plugin -- must be global |
| `pr-review` | `security-auditor` | NOT in TGOSKits plugin -- must be global |
| `driver-audit` | `security-auditor` | NOT in TGOSKits plugin -- must be global |
| `impl` | `test-gen` | Local (TGOSKits) -- OK |
| `impl` | `pr-review` | Local (TGOSKits) -- OK |
| `impl` | `security-auditor` | NOT in TGOSKits plugin -- must be global |

**Failure modes:**

1. **Agent not found**: If `security-auditor` or `debugger` are not installed as global agents, the spawn call fails. The agent may silently skip the security review step and produce incomplete results.

2. **Timeout**: If the spawned agent hangs (e.g., waiting for user input in a non-interactive context), the parent agent may wait indefinitely or crash.

3. **Conflicting instructions**: If `security-auditor` has its own CLAUDE.md or agent rules that contradict TGOSKits agent instructions, the spawned agent may produce results that don't align with TGOSKits expectations.

### Warning signs

- Agent reports "spawning security-auditor" but the subagent never produces output
- Agent completes faster than expected (skipped the subagent step)
- Subagent output references different coding standards or conventions than TGOSKits

### Prevention strategies

1. **Verify agent existence** before spawning: check if the agent is registered
2. **Set explicit timeouts** for subagent tasks
3. **Provide explicit context** to spawned agents (TGOSKits conventions, expected output format)
4. **Add a fallback**: if `security-auditor` is unavailable, the parent agent should perform a manual security review
5. **Document required global agents** in CLAUDE.md or plugin README

### Which phase should address

All phases that spawn subagents (bug-hunt, pr-review, driver-audit, impl)

### Testing strategy

```bash
# Test: invoke agent without the required global agents installed
# Check that it (a) detects the missing agent and (b) falls back to manual review
# Test: invoke agent with the global agent installed
# Check that subagent output is consumed correctly
```

---

## PITFALL 4: Hook Chain Conflicts and Ordering

### What goes wrong

TGOSKits registers 4 hooks in `hooks.json`:
- PreToolUse (Bash): `docker-check.py`
- PreToolUse (Bash): `pre-pr-gate.py`
- PostToolUse (Edit|Write): `post-tool-use-log.py`
- Stop: `stop-hook.py`

**Failure scenarios:**

1. **Multiple PreToolUse hooks on Bash**: Both hooks must complete. If `docker-check.py` exits 0 but `pre-pr-gate.py` exits 1, the Bash command is blocked. However, if another plugin registers a PreToolUse hook on `Edit|Write` that exits 1, the Edit/Write is blocked -- but the POST-hook may have already been bypassed. The interaction between PRE-block and POST-execution is subtle.

2. **Hook ordering is undefined**: If plugin A and plugin B both register PreToolUse on Bash, the execution order is not guaranteed. If `docker-check.py` assumes Docker is available but another hook runs first and modifies the environment, `docker-check.py` may fail spuriously.

3. **Hook timeout**: Each hook has an implicit timeout. If a hook takes too long (e.g., `pre-pr-gate.py` does `git fetch upstream dev` over a slow network), it may be killed. If killed, does the Bash command proceed or get blocked? Behavior is implementation-dependent.

4. **Stop hook conflicts**: If another plugin also registers a Stop hook, both will fire. If the other plugin's Stop hook runs first and modifies state (e.g., cleans up cache files), TGOSKits' `stop-hook.py` may read stale or missing data.

### Warning signs

- Commands that worked before suddenly being blocked
- Hook scripts failing with timeout errors
- Journal generation failing intermittently (stop hook race condition)

### Prevention strategies

1. **Idempotent hooks**: All hook scripts should be safe to run multiple times and in any order
2. **No shared state between hooks**: Hooks should not depend on state set by other hooks
3. **Time-bounded operations**: Network operations in hooks should have short timeouts (already done: 30s timeout in `pre-pr-gate.py`)
4. **Hook failure should not block unrelated commands**: Consider more specific matchers (not just `Bash`)

### Which phase should address

`self-evolve` phase (D7 hook integration dimension)

### Testing strategy

```bash
# Test: install another plugin with PreToolUse hooks, verify both execute
# Test: simulate hook timeout (e.g., block network for pre-pr-gate.py)
# Test: verify Stop hook journal generation works when another Stop hook runs concurrently
```

---

## PITFALL 5: Tool Permission Boundaries Across Plugins

### What goes wrong

The project `.claude/settings.json` is currently `{}` (empty). All tool permissions come from the user's global settings (not visible in the project).

**Failure scenarios:**

1. **Agent declares tool but it's denied globally**: Agent frontmatter lists `tools: [Read, Write, Edit, Bash, Grep, Glob]` but if the user's global settings deny `Write` for security reasons, the agent cannot function. It will fail at the first `Write` or `Edit` call with a permission error -- possibly after doing half the work.

2. **New tool needed but not in allowlist**: If an agent uses `WebSearch` or `WebFetch` (like `impl` does), but the global settings don't allow these, the agent breaks mid-execution.

3. **MCP tool permissions are per-server, not per-plugin**: If the `context7` MCP server is configured in global mcp_config.json but requires an API key (as shown: `"api-key": "YOUR_API_KEY"`), the tool will fail. The agent has no way to detect this before trying to use it.

4. **Permission scope mismatch**: Project-local hook scripts (`docker-check.py`, `pre-pr-gate.py`) use `subprocess.run` to execute `git`, `docker`, etc. If the sandbox prevents subprocess execution, hooks fail silently (exit 0), defeating their purpose.

### Warning signs

- Agent fails mid-workflow with "permission denied" for a tool
- Agent skips steps that require tools not in the allowlist
- MCP tool calls return errors about missing API keys

### Prevention strategies

1. **Document required tools** in CLAUDE.md or a project setup guide
2. **Validate tool availability early**: Before starting a workflow, check that all needed tools are accessible
3. **Provide `.claude/settings.json` with explicit allowlist** for project-required tools
4. **MCP API key validation**: Add a startup check that verifies MCP servers are reachable

### Which phase should address

Project setup / onboarding phase (not currently covered by any agent)

### Testing strategy

```bash
# Test: run agent with restricted tool permissions
# Test: run agent with missing MCP API key
# Verify agent detects the issue early and reports clearly
```

---

## PITFALL 6: Version Drift of Referenced Plugins

### What goes wrong

TGOSKits currently depends on superpowers@5.1.0 (with an older 5.0.7 still cached). The `installed_plugins.json` shows 11 out of 19 installed plugins have version `"unknown"`.

**Failure scenarios:**

1. **Silent update**: `claude plugins update` pulls latest superpowers. If superpowers 6.0 changes skill names, signatures, or removes skills, TGOSKits agents break. No warning at update time because skill references are resolved at invocation time, not at install time.

2. **Multiple versions cached**: superpowers has both `5.0.7/` and `5.1.0/` in the cache. Which one does Claude Code use? If it always uses the latest, that's fine. But if version selection is non-deterministic, behavior varies between sessions.

3. **Unknown version plugins**: 11 plugins show version `"unknown"`. If there's no version metadata, there's no way to know if an update changed behavior or if the plugin is compatible.

4. **Marketplace updates**: Three marketplaces auto-update (last update timestamps show periodic refreshes). If a marketplace removes a plugin or changes its structure, installed plugins may become orphaned.

### Warning signs

- Plugin behavior changes between sessions without explicit update
- Skill invocation errors for previously-working skills
- Version "unknown" in installed_plugins.json

### Prevention strategies

1. **Lock file**: Maintain a project-level plugin lock file that pins exact versions
2. **CI validation**: CI should validate that all cross-plugin skill references resolve against the pinned versions
3. **Version-awareness in references**: Reference skills with explicit version: `superpowers@5.1.0:verification-before-completion` (if supported)
4. **Periodic compatibility check**: Part of `self-evolve` should verify that all cross-plugin references resolve against the currently installed versions

### Which phase should address

`self-evolve` phase (extends D4 cross-reference check to validate against actual installed plugin versions)

### Testing strategy

```bash
# Version snapshot: save sha256 of all plugin files
# After plugin update: re-run snapshot, diff, check all cross-references
find ~/.claude/plugins/cache -name "*.md" -o -name "*.json" | sort | xargs sha256sum > .claude/cache/plugin-snapshot.txt
```

---

## PITFALL 7: MCP Server Dependency Chain

### What goes wrong

TGOSKits agents reference `context7` MCP server for Linux man-pages and documentation. The mcp_config.json shows:

```json
"context7": {
    "command": "npx",
    "args": ["-y", "@upstash/context7-mcp", "--api-key", "YOUR_API_KEY"]
}
```

**Failure scenarios:**

1. **Missing API key**: The placeholder `YOUR_API_KEY` means the MCP server won't work. Agents that fall back to `WebSearch` are OK (`impl`, `pr-review`, `bug-hunt`, `test-gen` have this fallback). But `driver-audit` only mentions `context7` without a web search fallback.

2. **npx not available**: If Node.js/npm is not installed, the MCP server cannot start. This is a silent failure -- the agent won't know until it tries to call the tool.

3. **Network dependency**: `npx -y @upstash/context7-mcp` downloads the package on first use. If offline, the MCP server never starts.

4. **Multiple MCP servers with same tool names**: The `github` MCP plugin registers tools like `search_code`, `search_issues`. If another MCP server registers tools with the same names, conflicts occur. The `serena` plugin also registers `search_for_pattern` which could overlap.

### Warning signs

- Agent tries to use `context7` and gets no response or error
- Agent falls back to web search unexpectedly
- MCP tool invocation returns "server not connected" errors

### Prevention strategies

1. **Replace placeholder API keys** with actual keys or document that they must be set
2. **Validate MCP connectivity at session start**: check that all referenced MCP servers are running
3. **Always include a fallback** for every MCP tool dependency (web search, local docs, etc.)
4. **Document MCP prerequisites** in project setup guide

### Which phase should address

Project setup / onboarding, with `self-evolve` validation check

### Testing strategy

```bash
# Test: disable context7 MCP server, run each agent
# Verify agents detect the missing server and use fallback (WebSearch)
# Test: run with no internet access at all
# Verify agents fail gracefully, not silently
```

---

## PITFALL 8: Cross-Plugin Instruction Priority Conflicts

### What goes wrong

TGOSKits agents operate in an environment with MULTIPLE sources of behavioral rules:

1. **Project CLAUDE.md** -- `CLAUDE.md` in project root
2. **Global user rules** -- QWEN.md, CLAUDE.md, GEMINI.md, OPENCODE.md (4 different rule files for 4 different AI assistants)
3. **Superpowers skills** -- instruction priority says user instructions > superpowers > system prompt
4. **TGOSKits agent rules** -- the agent `.md` file itself
5. **TGOSKits local skills** -- project skills under `.claude/skills/`
6. **Other installed plugins** -- e.g., `hookify` hooks, `ralph-loop` workflow

**Failure scenarios:**

1. **Contradictory rules**: QWEN.md mandates "Socratic Gate" (must ask 3 questions before any code). A TGOSKits agent like `self-evolve` says "Fix ALL BLOCK items first" without asking. Which rule wins? The agent may get stuck in an infinite loop of asking vs. doing.

2. **Ralph Loop interference**: The `ralph-loop` plugin can create recursive task execution. If activated during a TGOSKits agent workflow (e.g., `impl`'s iterative CI loop), the two loops could interact unpredictably.

3. **Hookify blocking agent actions**: If `hookify` rules are configured to block certain bash commands (e.g., `git push --force`), but a TGOSKits agent attempts to run one, the agent may not understand why the command failed.

4. **Custom CLAUDE.md rules**: The user may add project-specific rules to CLAUDE.md that conflict with agent design (e.g., "never auto-fix code" contradicts `pr-review` agent's "auto-fix BLOCK items").

### Warning signs

- Agent seems confused, asking questions the agent rules say it should handle automatically
- Agent gets stuck in verification-ask-verify loops
- Commands fail with unexplained permission/hook errors

### Prevention strategies

1. **Document rule priority explicitly** in each agent file (none currently do this)
2. **Agent rules should acknowledge potentially overriding contexts**: "These rules apply unless the user has explicitly forbidden this behavior in CLAUDE.md"
3. **Test agents with various CLAUDE.md configurations**: especially with the Socratic Gate enabled
4. **Add a "Rule Conflict Resolution" section** to the project CLAUDE.md

### Which phase should address

`self-evolve` phase (D7 hook integration + new D8 cross-plugin rule conflict dimension)

### Testing strategy

```bash
# Test: run agent with restrictive CLAUDE.md rules
# Test: run agent with ralph-loop active
# Test: run agent with hookify blocking common commands
# Verify agent either respects rules or clearly reports the conflict
```

---

## PITFALL 9: Implicit Dependency on Global Agent Ecosystem

### What goes wrong

TGOSKits agents reference three agents that are NOT part of the TGOSKits plugin and are NOT explicitly listed as dependencies:
- `security-auditor` -- referenced by `bug-hunt`, `pr-review`, `driver-audit`, `impl`
- `debugger` -- referenced by `bug-hunt`
- `context7` MCP server -- referenced by all agents except `self-evolve`

None of these are declared in `plugin.json` as dependencies. There is no mechanism in plugin.json v0.1.0 for dependency declarations.

**Failure scenarios:**

1. **New contributor setup**: A new developer installs only the TGOSKits plugin. Agents that spawn `security-auditor` fail because the agent doesn't exist. The contributor doesn't know what's missing or how to install it.

2. **Dependency not documented**: CLAUDE.md doesn't list required global agents/plugins. The only way to discover the requirement is to read each agent file and trace the spawn calls.

3. **Security audit silently skipped**: If `security-auditor` is missing, the calling agent may skip the security review step without warning. The PR or fix proceeds without security validation.

### Warning signs

- Agent output lacks security review section
- Agent completes faster than documented workflow suggests
- No error about missing agent (silent skip)

### Prevention strategies

1. **Add a prerequisites section** to CLAUDE.md listing required global agents/plugins
2. **Add startup validation** to each agent: check that spawnable agents exist before starting workflow
3. **Explicit dependency declarations**: if/when plugin.json supports dependencies, list them
4. **Fail loudly, not silently**: if a required subagent is missing, the parent should report "ABORTED: security-auditor agent not available"

### Which phase should address

Project setup + `self-evolve` (new D4 check: verify all spawnable agents exist)

### Testing strategy

```bash
# Test: clone repo, install only TGOSKits plugin, run each agent
# Verify each agent either works with reduced functionality (documented) 
# or fails with a clear "missing dependency" message
```

---

## PITFALL 10: Post-Tool-Use Logging and Privacy

### What goes wrong

The PostToolUse hook on `Edit|Write` runs `post-tool-use-log.py` which logs every file modification. This is a TGOSKits-local hook that writes to `.claude/cache/`.

**Failure scenarios:**

1. **Plugin interaction**: The `hookify` plugin can also register PostToolUse hooks. If hookify inspects the same Edit|Write events and takes contradictory action (e.g., blocking certain file writes), the log may record the write attempt but the actual write was blocked. The log becomes inconsistent with reality.

2. **Performance impact**: Every Edit|Write triggers a Python script. If another plugin also hooks Edit|Write, the overhead multiplies. For bulk operations (many small edits), this can slow down the agent noticeably.

3. **Sensitive data logging**: If an agent writes a file containing API keys, credentials, or personal data, `post-tool-use-log.py` logs the path and possibly the content. This could leak secrets into the cache directory.

### Warning signs

- Log shows edits that "didn't happen" (blocked by another hook)
- Agent operations are noticeably slower than expected
- Cache directory contains sensitive information

### Prevention strategies

1. **Minimal logging**: Log only file paths, not content
2. **Add .gitignore for cache**: Ensure `.claude/cache/` is gitignored
3. **Hook performance budget**: Monitor hook execution time; if >100ms per invocation, optimize or make async
4. **Configurable logging level**: Allow disabling detailed logging for sensitive workflows

### Which phase should address

`self-evolve` phase (security + performance dimension)

### Testing strategy

```bash
# Test: measure hook execution time with strace
# Test: verify .claude/cache/ is in .gitignore
# Test: write a file with fake secrets, verify log doesn't contain them
```

---

## Summary: Pitfall Severity Matrix

| # | Pitfall | Severity | Detectability | Current Mitigation | Phase |
|---|---|---|---|---|---|
| 1 | Cross-plugin skill resolution | **HIGH** | Low (silent) | None | self-evolve |
| 2 | Agent/command name collisions | **MEDIUM** | Medium (load-time) | None | self-evolve |
| 3 | Agent delegation failures | **HIGH** | Low (silent skip) | Fallback missing | bug-hunt, pr-review, impl |
| 4 | Hook chain conflicts | **LOW** | Medium | Idempotent design helps | self-evolve |
| 5 | Tool permission boundaries | **MEDIUM** | Low (late failure) | None | Setup/onboarding |
| 6 | Version drift | **MEDIUM** | Low (cumulative) | None | self-evolve |
| 7 | MCP server dependency | **MEDIUM** | Medium (error at call) | WebSearch fallback | Setup + self-evolve |
| 8 | Cross-plugin instruction conflicts | **HIGH** | Medium (confusion) | None | self-evolve |
| 9 | Implicit global agent dependency | **HIGH** | Low (silent skip) | None | Setup + self-evolve |
| 10 | Post-tool-use logging risks | **LOW** | Low | .gitignore likely | self-evolve |

---

## Recommended Immediate Actions

1. **Document required global agents** in CLAUDE.md (Pitfall 9)
2. **Add missing-agent detection** in agents that spawn subagents (Pitfall 3)
3. **Validate all superpowers skill references** resolve against superpowers@5.1.0 (Pitfall 1)
4. **Replace or remove the placeholder MCP API key** in mcp_config.json (Pitfall 7)
5. **Add a rule conflict awareness section** to CLAUDE.md (Pitfall 8)
6. **Add namespace prefix** to local agent/command names: `tg-` (Pitfall 2)
