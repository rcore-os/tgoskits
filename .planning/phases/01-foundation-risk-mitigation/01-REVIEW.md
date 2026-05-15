---
phase: 01-foundation-risk-mitigation
reviewed: 2026-05-13T17:30:00Z
depth: standard
files_reviewed: 11
files_reviewed_list:
  - .claude/scripts/validate-deps.py
  - .claude/scripts/test_validate_deps.py
  - .claude/hooks/hooks.json
  - .claude/scripts/test_preamble_consistency.sh
  - .claude/scripts/test_frontmatter_tools.sh
  - .claude/agents/pr-review.md
  - .claude/agents/bug-hunt.md
  - .claude/agents/impl.md
  - .claude/agents/driver-audit.md
  - .claude/agents/test-gen.md
  - .claude/agents/self-evolve.md
findings:
  critical: 2
  warning: 3
  info: 2
  total: 7
status: issues_found
---

# Phase 01: Code Review Report

**Reviewed:** 2026-05-13T17:30:00Z
**Depth:** standard
**Files Reviewed:** 11
**Status:** issues_found

## Summary

Reviewed 11 source files from the TGOSKits `.claude/` plugin: 2 validation/shell scripts, 1 Python test module, 1 JSON hook config, and 6 agent definition files plus 1 self-evolve meta-agent. Two critical findings were identified: a dependency on a non-existent `debugger` agent that would cause `bug-hunt` to always abort, and a correctness bug in `test_frontmatter_tools.sh` that validates body content instead of frontmatter. Three warnings were raised for fragile parsing and edge-case handling. No security vulnerabilities (injection, hardcoded secrets) were found.

---

## Critical Issues

### CR-01: bug-hunt agent references non-existent `debugger` agent -- always aborts

**File:** `.claude/agents/bug-hunt.md:33`
**Issue:** The Dependency Check section declares `debugger` as a required spawnable agent:
```
**Agents** (must be spawnable):
- `debugger` — complex debugging, crashes, multi-core races
```
However, no `debugger.md` file exists in `.claude/agents/`, and `debugger` is not registered in `.claude/plugin.json`'s `agents` array. The required plugins (`superpowers`, `pr-review-toolkit`) do not provide a `debugger` agent either. This causes the bug-hunt agent to **always abort** when its Dependency Check is enforced, rendering it unusable.

Additionally, the bug-hunt workflow (Phase 1 Step 1, line 33) references launching the `pr-review` agent (Phase 5 Step 3, line 386) for self-review. While `pr-review` exists, the initial `debugger` reference blocks all progress before reaching that stage.

**Fix:** Either:
1. Create a `.claude/agents/debugger.md` agent file and register it in `plugin.json`, or
2. Remove `debugger` from the `**Agents**` required list in `bug-hunt.md` and downgrade the debugging guidance to a skill-based approach using `superpowers:systematic-debugging` (which is already in the required skills).

Recommended fix (option 2 -- minimal, no new agent needed):
```markdown
# In bug-hunt.md, change line 32-33 from:
**Agents** (must be spawnable):
- `debugger` — complex debugging, crashes, multi-core races

# To:
**Agents** (must be spawnable):
- None (complex debugging handled by `superpowers:systematic-debugging` skill)
```

---

### CR-02: test_frontmatter_tools.sh validates body content instead of frontmatter

**File:** `.claude/scripts/test_frontmatter_tools.sh:19-23`
**Issue:** The `extract_yaml_field` function uses a sed range `/^${field}:/,/^[a-z]/` to extract a YAML field. However, this range does not stop at the frontmatter closing `---` delimiter. It extends through the entire file body until it finds a line starting with a lowercase letter. In agent files like `pr-review.md`, body content (the Dependency Check section, headings, prose) contains the same keywords (`WebSearch`, `WebFetch`, `superpowers:systematic-debugging`) that the test is supposed to verify exist in the frontmatter. The test currently passes -- but validates body content, not frontmatter content.

This means: if someone removes `WebSearch` from the frontmatter `tools:` list but keeps it in the Dependency Check body section, the test still passes. The test provides a false sense of correctness.

**Fix:** Modify the range to stop at the frontmatter closing `---`:
```bash
extract_yaml_field() {
  local file="$1"
  local field="$2"
  # Extract from "field:" to the closing "---" of frontmatter,
  # then remove the closing "---" line itself.
  sed -n "/^${field}:/,/^---$/p" "$file" | sed '$d'
}
```

This restricts extraction strictly to the YAML frontmatter block. The `---` closing delimiter is the canonical boundary marker.

---

## Warnings

### WR-01: validate-deps.py crashes on malformed non-list plugin entries

**File:** `.claude/scripts/validate-deps.py:90`
**Issue:** The code `entries[0].get("version", "unknown")` assumes `entries` is always a list. If `installed_plugins.json` has a malformed structure where a plugin entry is a dict instead of a list (e.g., `"superpowers@...": {"version": "5.1.0"}` instead of `[{...}]`), then `entries[0]` on a dict raises a `KeyError`, which is **not** caught. The only exception handler in `check_plugins()` is for `json.JSONDecodeError`. This crashes the SessionStart hook rather than producing a helpful error message.

**Fix:** Add a type guard before accessing `entries[0]`:
```python
entries = installed.get(plugin_key, [])
if not entries or not isinstance(entries, list):
    missing.append((plugin_key, "not installed", req["purpose"]))
    continue
```

---

### WR-02: test_preamble_consistency.sh uses `echo` for potentially multi-line body content

**File:** `.claude/scripts/test_preamble_consistency.sh:56`
**Issue:** The preamble extraction passes `$body` through `echo`:
```bash
preamble_section=$(echo "$body" | sed -n ...)
```
In POSIX shells, `echo` behavior with backslash characters is implementation-defined. While the current agent file bodies do not contain literal backslash sequences that would be mangled, using `echo` on arbitrary multi-line text is a latent portability bug. If any agent file adds a code block with backslash escapes (e.g., `\n`, `\t`), the preamble section extraction would silently corrupt the content.

**Fix:** Use `printf` which has well-defined behavior:
```bash
preamble_section=$(printf '%s\n' "$body" | sed -n '/^### Dependency Check/,/^#/{/^### Dependency Check/p;/^#/!p}' | head -n -1)
```

---

### WR-03: self-evolve D3 name check only inspects first 5 lines of each file

**File:** `.claude/agents/self-evolve.md:114-118`
**Issue:** The D3 frontmatter name validation command uses `head -5`:
```bash
for f in .claude/agents/*.md .claude/commands/*.md; do
  name=$(head -5 "$f" | grep '^name:' | sed 's/name: *//')
  ...
done
```
If a YAML frontmatter includes a comment or a multi-line description field that pushes the `name:` field to line 6 or later, this check silently fails (extracts empty string, reports false mismatch or skips validation entirely). While all current files have `name:` on line 2, the check is not robust against future edits.

**Fix:** Search the entire frontmatter block, not just the first 5 lines:
```bash
for f in .claude/agents/*.md .claude/commands/*.md; do
  name=$(sed -n '/^---$/,/^---$/p' "$f" | grep '^name:' | head -1 | sed 's/^name: *//')
  ...
done
```

---

## Info

### IN-01: test_preamble_consistency.sh preamble range extraction is fragile

**File:** `.claude/scripts/test_preamble_consistency.sh:56`
**Issue:** The preamble section is extracted by a complex sed chain:
```bash
preamble_section=$(echo "$body" | sed -n '/^### Dependency Check/,/^#/{/^### Dependency Check/p;/^#/!p}' | head -n -1)
```
The range terminus `^#` matches any heading line (e.g., `# Agent`, `## Section`). If an agent adds a sub-heading (starting with `#`) within the preamble section itself -- before the main `# Agent Title` heading -- the range truncates prematurely and Checks 3 and 4 fail on the truncated preamble. The logic implicitly assumes the first `#` line always marks the end of the preamble, but this is a convention, not a structural guarantee.

No action required unless the preamble format evolves. Documented here for awareness when modifying agent file structure.

---

### IN-02: validate-deps.py `parse_version` silently coerces all parse failures to `(0,)`

**File:** `.claude/scripts/validate-deps.py:26-41`
**Issue:** The `parse_version` function catches both `ValueError` and `AttributeError` and returns `(0,)`, which satisfies any minimum version check. This is intentionally lenient for the `"unknown"` version sentinel, but it also silently swallows real data corruption (e.g., version `"5.x.0"` or `"five.one.zero"` would pass version checks). Considered an INFO-level finding because the installed_plugins.json is machine-generated and unlikely to contain corrupted version strings, but the leniency could mask issues if the plugin manifest format changes upstream.

**Fix (optional):** Log a warning to stderr when a non-"unknown" version string fails to parse:
```python
if ver_str == "unknown":
    return (0,)
try:
    return tuple(int(x) for x in ver_str.split("."))
except (ValueError, AttributeError):
    print(f"WARNING: unparseable version string '{ver_str}', treating as (0,)", file=sys.stderr)
    return (0,)
```

---

_Reviewed: 2026-05-13T17:30:00Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
