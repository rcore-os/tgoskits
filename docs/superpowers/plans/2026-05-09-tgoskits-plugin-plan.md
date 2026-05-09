# TGOSKits Plugin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement a project-local Claude Code plugin for TGOSKits that provides Docker-based local CI, automated hooks for logging and PR gates, slash commands for testing and PR workflow, and specialized agents for bug hunting, PR review, test generation, and driver auditing.

**Architecture:** Four batches. Batch 1 (Foundation) establishes the plugin manifest, settings, Docker CI config, and the `local-ci.sh` script — all later components depend on this. Batch 2 (Hooks) adds event-driven logging, PR gating, and journal generation. Batch 3 (Commands) delivers `/test` and `/pr-prep` as user-facing entry points. Batch 4 (Agents) implements the four specialized sub-agents and the syscall-diff infrastructure.

**Tech Stack:** Bash (local-ci.sh), Python 3 (syscall-diff.py, journal-generator.py, hook scripts), Markdown + YAML frontmatter (commands, agents, hooks), TOML (config), JSON (plugin manifest, settings).

---

## Batch 1: Plugin Foundation + Docker CI Infrastructure

### Task 1.1: Create plugin.json manifest

**Files:**
- Create: `.claude/plugin.json`

- [ ] **Step 1: Write plugin.json**

```json
{
  "name": "tgoskits",
  "description": "TGOSKits project-local plugin — local CI, PR workflow, bug hunting, and driver auditing for OS/kernel development",
  "version": "0.1.0",
  "commands": [
    "commands/test.md",
    "commands/pr-prep.md"
  ],
  "agents": [
    "agents/pr-review.md",
    "agents/test-gen.md",
    "agents/bug-hunt.md",
    "agents/driver-audit.md"
  ],
  "hooks": [
    "hooks/post-tool-use-log.md",
    "hooks/pre-pr-gate.md",
    "hooks/session-end-journal.md"
  ]
}
```

- [ ] **Step 2: Commit**

```bash
git add .claude/plugin.json
git commit -m "feat(plugin): add plugin.json manifest for tgoskits project plugin"
```

### Task 1.2: Create settings.json with hook registrations

**Files:**
- Create: `.claude/settings.json`

- [ ] **Step 1: Write settings.json**

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "prompt",
            "prompt": "Before executing this Bash command, check if it matches gh pr create or git push. If it does, read .claude/hooks/pre-pr-gate.md and follow its instructions. If the gate checks fail, block the command and tell the user what needs to be fixed."
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "python3 \"${CLAUDE_PLUGIN_ROOT}/scripts/post-tool-use-log.py\" \"${CLAUDE_PLUGIN_ROOT}\""
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "prompt",
            "prompt": "Check if .claude/cache/task-active.flag exists. If it does, read the task name from it, then read .claude/hooks/session-end-journal.md and follow its instructions to generate the journal. Do NOT do this if the flag does not exist or if a journal was already generated for this task."
          }
        ]
      }
    ]
  }
}
```

- [ ] **Step 2: Commit**

```bash
git add .claude/settings.json
git commit -m "feat(plugin): add settings.json with hook registrations"
```

### Task 1.3: Create docker-ci.toml configuration

**Files:**
- Create: `.claude/config/docker-ci.toml`

- [ ] **Step 1: Write config/docker-ci.toml**

```toml
[base_image]
name = "tgoskits-ci"
dockerfile = "container/Dockerfile"
remote = "ghcr.io/seek-hope/tgoskits-container:latest"
rebuild_triggers = ["container/Dockerfile", "rust-toolchain.toml"]

[axvisor_lvz_image]
name = "tgoskits-ci-lvz"
dockerfile = "container/Dockerfile.axvisor-lvz"
remote = "ghcr.io/seek-hope/tgoskits-container-axvisor-lvz:latest"
rebuild_triggers = ["container/Dockerfile.axvisor-lvz", "container/Dockerfile", "rust-toolchain.toml"]

[quick]
commands = [
    "cargo fmt --all -- --check",
    "cargo xtask clippy",
    "cargo xtask sync-lint",
]

[full]
commands = [
    "cargo fmt --all -- --check",
    "cargo xtask clippy",
    "cargo xtask sync-lint",
    "cargo xtask test",
    "cargo xtask arceos test qemu --arch x86_64",
    "cargo xtask arceos test qemu --arch riscv64",
    "cargo xtask arceos test qemu --arch aarch64",
    "cargo xtask arceos test qemu --arch loongarch64",
    "cargo xtask starry test qemu --arch riscv64",
    "cargo xtask starry test qemu --arch aarch64",
    "cargo xtask starry test qemu --arch x86_64",
    "cargo xtask starry test qemu --arch loongarch64",
    "cargo xtask axvisor test qemu --arch aarch64",
    "cargo xtask axvisor test qemu --arch riscv64",
    "cargo xtask axvisor test qemu --arch loongarch64",
]

[pre_pr_gate]
require_local_ci = true
require_clean_base = true
block_direct_push = true
```

- [ ] **Step 2: Commit**

```bash
git add .claude/config/docker-ci.toml
git commit -m "feat(plugin): add docker-ci.toml with CI matrix and gate config"
```

### Task 1.4: Create local-ci.sh — Docker image management and test runner

**Files:**
- Create: `.claude/scripts/local-ci.sh`

- [ ] **Step 1: Write scripts/local-ci.sh**

```bash
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"
CONFIG_FILE="$PLUGIN_ROOT/config/docker-ci.toml"
CACHE_DIR="$PLUGIN_ROOT/cache"
WORKSPACE="$(git rev-parse --show-toplevel)"

mkdir -p "$CACHE_DIR"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
die() { echo "ERROR: $*" >&2; exit 1; }
warn() { echo "WARN: $*" >&2; }

toml_get() {
    # Crude TOML value extractor for our simple config structure.
    # Usage: toml_get "base_image.name" < "$CONFIG_FILE"
    local key="$1" section="" line
    while IFS= read -r line; do
        case "$line" in
            \[*\]) section="${line//\[/}"; section="${section//\]/}" ;;
            *=*)
                local k="${line%%=*}" v="${line#*=}"
                k="${k// /}"; v="${v// /}"; v="${v//\"/}"
                if [ "$key" = "$section.$k" ]; then echo "$v"; return 0; fi
                ;;
        esac
    done
    return 1
}

image_exists() {
    docker image inspect "$1" >/dev/null 2>&1
}

compute_hash() {
    local img="$1"
    local triggers="" file
    local section="${img}_image"
    triggers=$(grep -A10 "\[${section}\]" "$CONFIG_FILE" | grep "rebuild_triggers" | cut -d\" -f2 | tr ',' '\n' | sed 's/[][]//g' | sed 's/"//g' | sed 's/ //g')
    local hash_input=""
    while IFS= read -r f; do
        [ -n "$f" ] && [ -f "$WORKSPACE/$f" ] && hash_input+=$(sha256sum "$WORKSPACE/$f")
    done <<< "$triggers"
    echo "$hash_input" | sha256sum | cut -d' ' -f1
}

remote_exists() {
    local remote="$1"
    docker manifest inspect "$remote" >/dev/null 2>&1
}

push_image() {
    local name="$1" remote="$2"
    if [ -z "${GITHUB_TOKEN:-}" ] && [ -z "${CR_PAT:-}" ]; then
        warn "No GITHUB_TOKEN or CR_PAT set. Skipping push of $name to $remote."
        return 0
    fi
    echo "$GITHUB_TOKEN" | docker login ghcr.io -u seek-hope --password-stdin 2>/dev/null || true
    docker tag "$name" "$remote"
    docker push "$remote"
    echo "Pushed $name → $remote"
}

# ---------------------------------------------------------------------------
# Image management
# ---------------------------------------------------------------------------
ensure_image() {
    local img_section="${1}_image"
    local name dockerfile remote triggers hash_file hash

    name=$(toml_get "${img_section}.name" < "$CONFIG_FILE")
    dockerfile=$(toml_get "${img_section}.dockerfile" < "$CONFIG_FILE")
    remote=$(toml_get "${img_section}.remote" < "$CONFIG_FILE")
    hash_file="$CACHE_DIR/docker-image-${1}.hash"
    hash=$(compute_hash "$1")

    # Rebuild triggered?
    if [ -f "$hash_file" ] && [ "$(cat "$hash_file")" != "$hash" ]; then
        echo "[$name] Trigger files changed, rebuilding..."
        if docker build -t "$name" -f "$WORKSPACE/$dockerfile" "$WORKSPACE" --cache-from "$name"; then
            echo "$hash" > "$hash_file"
            push_image "$name" "$remote"
        else
            warn "Build failed, using existing local image"
        fi
        return 0
    fi

    # Local image exists?
    if image_exists "$name"; then
        echo "[$name] Using local image"
        return 0
    fi

    # No local image → build
    echo "[$name] No local image, building..."
    if docker build -t "$name" -f "$WORKSPACE/$dockerfile" "$WORKSPACE"; then
        echo "$hash" > "$hash_file"
        # Compare with remote
        if remote_exists "$remote"; then
            local remote_hash
            remote_hash=$(docker manifest inspect "$remote" 2>/dev/null | grep -o '"digest":"[^"]*"' | head -1 | cut -d'"' -f4)
            if [ -n "$remote_hash" ] && [ "$hash" != "$remote_hash" ]; then
                echo "[$name] Remote differs, pushing local..."
                push_image "$name" "$remote"
            else
                echo "[$name] Remote matches, no push needed"
            fi
        else
            echo "[$name] No remote, pushing..."
            push_image "$name" "$remote"
        fi
    else
        warn "Build failed, falling back to remote..."
        if remote_exists "$remote"; then
            docker pull "$remote"
            docker tag "$remote" "$name"
        else
            die "Cannot build or pull $name. Aborting."
        fi
    fi
}

# ---------------------------------------------------------------------------
# Running commands
# ---------------------------------------------------------------------------
run_in_container() {
    local image="$1" cmd="$2"
    echo "=== [$image] $cmd ==="
    docker run --rm -v "$WORKSPACE:/workspace" -w /workspace "$image" bash -c "$cmd"
}

# ---------------------------------------------------------------------------
# Main dispatch
# ---------------------------------------------------------------------------
cmd_quick() {
    ensure_image "base"
    local commands
    commands=$(grep -A20 '\[quick\]' "$CONFIG_FILE" | grep '^"' | sed 's/^[[:space:]]*"//;s/",\?$//')
    while IFS= read -r c; do
        [ -z "$c" ] && continue
        run_in_container "tgoskits-ci" "$c" || { echo "FAIL: $c"; return 1; }
    done <<< "$commands"
    echo "ALL QUICK CHECKS PASSED"
}

# shellcheck disable=SC2120
cmd_full() {
    ensure_image "base"
    ensure_image "axvisor_lvz"

    # Determine which image to use for loongarch64 tests
    # axvisor loongarch64 needs lvz image; others use base
    local failed=0
    local commands
    commands=$(grep -A30 '\[full\]' "$CONFIG_FILE" | grep '^"' | sed 's/^[[:space:]]*"//;s/",\?$//')

    while IFS= read -r c; do
        [ -z "$c" ] && continue
        local img="tgoskits-ci"
        # Use lvz image for axvisor loongarch64
        if echo "$c" | grep -q "axvisor.*loongarch64"; then
            img="tgoskits-ci-lvz"
        fi
        run_in_container "$img" "$c" || { echo "FAIL: $c"; failed=1; }
    done <<< "$commands"

    if [ "$failed" -eq 0 ]; then
        echo "ALL FULL CI CHECKS PASSED"
    else
        echo "SOME CHECKS FAILED"
        return 1
    fi
}

cmd_test() {
    local os="$1" arch="$2"
    ensure_image "base"
    local container_img="tgoskits-ci"
    local cmd="cargo xtask ${os} test qemu --arch ${arch}"
    if [ "$os" = "axvisor" ] && [ "$arch" = "loongarch64" ]; then
        ensure_image "axvisor_lvz"
        container_img="tgoskits-ci-lvz"
    fi
    run_in_container "$container_img" "$cmd"
}

cmd_rebuild() {
    local push="${1:-}"
    local hash

    echo "Rebuilding base image..."
    hash=$(compute_hash "base")
    docker build -t tgoskits-ci -f "$WORKSPACE/container/Dockerfile" "$WORKSPACE" --no-cache
    echo "$hash" > "$CACHE_DIR/docker-image-base.hash"

    echo "Rebuilding axvisor-lvz image..."
    hash=$(compute_hash "axvisor_lvz")
    docker build -t tgoskits-ci-lvz -f "$WORKSPACE/container/Dockerfile.axvisor-lvz" "$WORKSPACE" --no-cache
    echo "$hash" > "$CACHE_DIR/docker-image-lvz.hash"

    # Validate
    echo "Validating images..."
    run_in_container "tgoskits-ci" "cargo xtask arceos qemu --package ax-helloworld --arch aarch64" \
        || die "Base image validation failed"

    if [ "$push" = "--push" ]; then
        local base_remote lvz_remote
        base_remote=$(toml_get "base_image.remote" < "$CONFIG_FILE")
        lvz_remote=$(toml_get "axvisor_lvz_image.remote" < "$CONFIG_FILE")
        push_image "tgoskits-ci" "$base_remote"
        push_image "tgoskits-ci-lvz" "$lvz_remote"
    fi
    echo "Rebuild complete"
}

# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
usage() {
    cat <<'EOF'
Usage: local-ci.sh <command>

Commands:
  full                 Run full CI matrix (all arches, all OSes)
  quick                Run quick checks (fmt + clippy + sync-lint)
  test <os> <arch>     Run single-arch QEMU test
  rebuild              Force rebuild both Docker images
  rebuild --push       Force rebuild + validate + push to remote

Examples:
  ./scripts/local-ci.sh quick
  ./scripts/local-ci.sh test starry aarch64
  ./scripts/local-ci.sh full
  ./scripts/local-ci.sh rebuild --push
EOF
}

case "${1:-}" in
    full)      cmd_full ;;
    quick)     cmd_quick ;;
    test)      cmd_test "$2" "$3" ;;
    rebuild)   cmd_rebuild "${2:-}" ;;
    *)         usage; exit 1 ;;
esac
```

- [ ] **Step 2: Make executable, verify syntax**

```bash
chmod +x .claude/scripts/local-ci.sh
bash -n .claude/scripts/local-ci.sh
```
Expected: No syntax errors.

- [ ] **Step 3: Commit**

```bash
git add .claude/scripts/local-ci.sh
git commit -m "feat(plugin): add local-ci.sh — Docker image management and CI runner"
```

### Task 1.5: Create cache directory with .gitkeep

**Files:**
- Create: `.claude/cache/.gitkeep`

- [ ] **Step 1: Create cache directory and .gitkeep**

```bash
mkdir -p .claude/cache
touch .claude/cache/.gitkeep
```

- [ ] **Step 2: Update .gitignore**

Add to `.gitignore` (or create if not present):
```
.claude/cache/*.json
.claude/cache/*.hash
.claude/cache/*.flag
```

- [ ] **Step 3: Commit**

```bash
git add .claude/cache/.gitkeep .gitignore
git commit -m "chore(plugin): add cache directory with gitignore rules"
```

---

## Batch 2: Hooks + Journal Generator

### Task 2.1: Create post-tool-use-log.py — activity logger

**Files:**
- Create: `.claude/scripts/post-tool-use-log.py`

- [ ] **Step 1: Write scripts/post-tool-use-log.py**

```python
#!/usr/bin/env python3
"""PostToolUse hook: append activity summary to log.md."""
import os
import sys
from datetime import datetime, timezone

PLUGIN_ROOT = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WORKSPACE = os.path.dirname(PLUGIN_ROOT)
LOG_PATH = os.path.join(WORKSPACE, "log.md")

def get_changed_files():
    """Get list of files modified in working tree (staged + unstaged)."""
    import subprocess
    try:
        result = subprocess.run(
            ["git", "diff", "--name-only", "HEAD"],
            capture_output=True, text=True, cwd=WORKSPACE
        )
        staged = subprocess.run(
            ["git", "diff", "--name-only", "--cached"],
            capture_output=True, text=True, cwd=WORKSPACE
        )
        files = set()
        if result.stdout:
            files.update(result.stdout.strip().split("\n"))
        if staged.stdout:
            files.update(staged.stdout.strip().split("\n"))
        return sorted(f for f in files if f)
    except Exception:
        return []

def get_last_commit_message():
    """Get the last commit's subject line as a hint for the summary."""
    import subprocess
    try:
        result = subprocess.run(
            ["git", "log", "-1", "--format=%s"],
            capture_output=True, text=True, cwd=WORKSPACE
        )
        return result.stdout.strip()
    except Exception:
        return ""

def append_log(files, summary):
    """Append an entry to log.md."""
    now = datetime.now(timezone.utc)
    timestamp = now.strftime("%Y-%m-%d %H:%M")
    count = len(files)
    file_list = ", ".join(f"`{f}`" for f in files[:10])
    if len(files) > 10:
        file_list += f" (+{len(files) - 10} more)"
    summary = summary[:500]  # truncate

    entry = f"""## {timestamp} — {count} file{'s' if count != 1 else ''} changed

**Files**: {file_list}

**Summary**: {summary}

---
"""
    with open(LOG_PATH, "a") as f:
        f.write(entry)

if __name__ == "__main__":
    files = get_changed_files()
    if not files:
        sys.exit(0)

    commit_msg = get_last_commit_message()
    # Use commit message as summary; the AI is expected to write meaningful commits
    summary = commit_msg if commit_msg else "Code changes (see git log for details)"

    append_log(files, summary)
```

- [ ] **Step 2: Test with sample data**

```bash
python3 .claude/scripts/post-tool-use-log.py .claude
cat log.md
```
Expected: `log.md` exists with an entry.

- [ ] **Step 3: Commit**

```bash
git add .claude/scripts/post-tool-use-log.py
git commit -m "feat(plugin): add post-tool-use-log.py — append activity to log.md"
```

### Task 2.2: Create pre-pr-gate.md — PR gate hook prompt

**Files:**
- Create: `.claude/hooks/pre-pr-gate.md`

- [ ] **Step 1: Write hooks/pre-pr-gate.md**

```markdown
---
name: pre-pr-gate
description: Block PR creation and direct push unless clean branch + local CI pass
type: hook
---

# Pre-PR Gate

When the user (or you, the AI) attempts to run `gh pr create` or `git push` (to origin), you MUST run these checks before allowing the command to proceed.

## Gate Checks

### Check 1: Clean Base Branch

Run:
```bash
git fetch upstream dev 2>/dev/null || git fetch origin dev
UPSTREAM_HEAD=$(git rev-parse upstream/dev 2>/dev/null || git rev-parse origin/dev)
MERGE_BASE=$(git merge-base HEAD "$UPSTREAM_HEAD")
CURRENT_BASE=$(git rev-parse "$UPSTREAM_HEAD")
```

If `MERGE_BASE` does not equal `CURRENT_BASE`, the branch is not based on the latest dev.

**BLOCK the command.** Tell the user:
> "Branch is not based on upstream/dev HEAD. Please create a clean branch first:"
> ```
> git fetch upstream dev
> git checkout -b <feature-branch> upstream/dev
> ```

### Check 2: Local CI Passed

Check if `.claude/cache/last-ci-result.json` exists and contains `"status": "pass"`.

If it does not exist or status is not "pass":

**BLOCK the command.** Tell the user:
> "Local CI has not passed. Please run at minimum:"
> ```
> ./scripts/local-ci.sh quick
> ```

### Check 3: Direct Push (warning only for non-feature branches)

If the command is `git push` and the target branch is `main` or `dev`:

**BLOCK the command.** Tell the user:
> "Direct push to main/dev is forbidden. Use a feature branch and create a PR."

## If All Checks Pass

Allow the command to execute.
```

- [ ] **Step 2: Commit**

```bash
git add .claude/hooks/pre-pr-gate.md
git commit -m "feat(plugin): add pre-pr-gate.md hook — PR/push gate"
```

### Task 2.3: Create session-end-journal.md — journal generation hook

**Files:**
- Create: `.claude/hooks/session-end-journal.md`

- [ ] **Step 1: Write hooks/session-end-journal.md**

```markdown
---
name: session-end-journal
description: Generate task journal from log.md at session end
type: hook
---

# Session-End Journal Generator

When triggered (session Stop event AND `.claude/cache/task-active.flag` exists), generate a journal file.

## Steps

### 1. Read task name

```bash
cat .claude/cache/task-active.flag
```

The flag file contains the task name on a single line (e.g., `fix-timer-syscalls`).

### 2. Read log.md entries

Read `log.md` and extract entries that were created since the task started.
If `.claude/cache/task-started-at.txt` exists, use its timestamp as the filter.
Otherwise, include all entries from this session.

### 3. Collect metadata

```bash
# Current branch
git branch --show-current

# Files touched (from log.md entries)
# Commit count since task start

# CI result
cat .claude/cache/last-ci-result.json 2>/dev/null || echo '{"status": "unknown"}'
```

### 4. Generate journal

Write `[task-name]-journal.md` with this structure:

```markdown
# Journal: <task-name>

**Time**: <start> ~ <end>
**Branch**: <branch-name>
**Files touched**: <count>

## Task Summary
<Ask the user or AI to provide a one-paragraph summary of what was accomplished>

## Change Log
<Copy the relevant entries from log.md>

## Test Results
<Paste CI results from last-ci-result.json>

## Key Decisions
<List any architectural or design decisions made during this task>

## Open Issues
<List anything left unfinished>
```

### 5. Clean up

Remove `.claude/cache/task-active.flag` and `.claude/cache/task-started-at.txt`.

Save the journal to the workspace root.
```

- [ ] **Step 2: Commit**

```bash
git add .claude/hooks/session-end-journal.md
git commit -m "feat(plugin): add session-end-journal.md hook — task journal generator"
```

### Task 2.4: Create journal-generator.py — programmatic journal generation

**Files:**
- Create: `.claude/scripts/journal-generator.py`

- [ ] **Step 1: Write scripts/journal-generator.py**

```python
#!/usr/bin/env python3
"""Generate [task-name]-journal.md from log.md and CI results."""
import os
import sys
import json
from datetime import datetime, timezone

PLUGIN_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WORKSPACE = os.path.dirname(PLUGIN_ROOT)
CACHE_DIR = os.path.join(PLUGIN_ROOT, "cache")
LOG_PATH = os.path.join(WORKSPACE, "log.md")


def read_log_entries():
    """Parse log.md into list of entries."""
    if not os.path.exists(LOG_PATH):
        return []
    with open(LOG_PATH) as f:
        content = f.read()
    entries = []
    for block in content.split("\n---\n"):
        block = block.strip()
        if block.startswith("## "):
            entries.append(block)
    return entries


def get_branch():
    import subprocess
    try:
        return subprocess.run(
            ["git", "branch", "--show-current"],
            capture_output=True, text=True, cwd=WORKSPACE
        ).stdout.strip()
    except Exception:
        return "unknown"


def count_files_touched(entries):
    """Count unique files from log entries."""
    import re
    files = set()
    for entry in entries:
        for match in re.finditer(r"`([^`]+)`", entry):
            f = match.group(1)
            if "/" in f or "." in f:
                files.add(f)
    return len(files)


def get_ci_result():
    ci_file = os.path.join(CACHE_DIR, "last-ci-result.json")
    if os.path.exists(ci_file):
        with open(ci_file) as f:
            return json.load(f)
    return {"status": "unknown", "results": []}


def generate_journal(task_name, entries, start_time=None):
    """Generate journal content."""
    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M")
    start = start_time or "unknown"
    branch = get_branch()
    file_count = count_files_touched(entries)
    ci = get_ci_result()

    journal = f"""# Journal: {task_name}

**Time**: {start} ~ {now}
**Branch**: {branch}
**Files touched**: {file_count}

## Task Summary
<!-- TODO: fill in -->

## Change Log
"""
    for entry in entries:
        journal += entry + "\n\n---\n\n"

    journal += "## Test Results\n"
    if ci["status"] == "pass":
        journal += "All CI checks passed.\n"
    elif ci.get("results"):
        for r in ci["results"]:
            status = "PASS" if r.get("pass") else "FAIL"
            journal += f"- {status}: {r.get('command', 'unknown')}\n"
    else:
        journal += "No CI results available.\n"

    journal += """
## Key Decisions
<!-- TODO: fill in -->

## Open Issues
<!-- TODO: fill in -->
"""
    return journal


if __name__ == "__main__":
    task_name = sys.argv[1] if len(sys.argv) > 1 else "task"
    entries = read_log_entries()
    journal = generate_journal(task_name, entries)
    output_path = os.path.join(WORKSPACE, f"{task_name}-journal.md")
    with open(output_path, "w") as f:
        f.write(journal)
    print(f"Journal written to {output_path}")
```

- [ ] **Step 2: Test with sample log.md**

```bash
echo '## 2026-05-09 14:32 — 2 files changed

**Files**: `foo.rs`, `bar.rs`

**Summary**: Test entry

---
' > log.md
python3 .claude/scripts/journal-generator.py test-task
cat test-task-journal.md
```
Expected: Journal file created with the test entry in Change Log section.

- [ ] **Step 3: Clean up test files and commit**

```bash
rm -f log.md test-task-journal.md
git add .claude/scripts/journal-generator.py
git commit -m "feat(plugin): add journal-generator.py — transform log.md to journal"
```

### Task 2.5: Create post-tool-use-log.md and session-end-journal.md hook prompts

Wait — these were already created in Tasks 2.2 and 2.3. The hook system uses the settings.json registration (Task 1.2) to wire them. The `.md` files serve as instructions for the AI when the hook fires.

The hooks are already complete from Tasks 2.2–2.4. Let's verify the whole Batch 2 is consistent.

- [ ] **Step 1: Verify hook files exist and are consistent**

```bash
ls -la .claude/hooks/
ls -la .claude/scripts/post-tool-use-log.py .claude/scripts/journal-generator.py
```
Expected: All 5 files exist.

- [ ] **Step 2: No additional commit needed** (all files committed in their respective tasks)

---

## Batch 3: Commands

### Task 3.1: Create /test command

**Files:**
- Create: `.claude/commands/test.md`

- [ ] **Step 1: Write commands/test.md**

```markdown
---
name: test
description: Run builds and tests in the Docker CI container
args:
  - name: scope
    type: string
    required: false
    default: quick
    enum: [quick, full, fmt, clippy, starry, arceos, axvisor]
  - name: arch
    type: string
    required: false
    default: all
    enum: [aarch64, riscv64, x86_64, loongarch64, all]
---

# /test — Run CI checks in Docker container

## Dispatch

| Invocation | Action |
|------------|--------|
| `/test` | Run quick checks (fmt + clippy + sync-lint) |
| `/test quick` | Run quick checks |
| `/test full` | Run full CI matrix (all OSes, all architectures) |
| `/test fmt` | Run `cargo fmt --all -- --check` |
| `/test clippy` | Run `cargo xtask clippy` |
| `/test starry aarch64` | Run StarryOS QEMU tests for aarch64 |
| `/test arceos riscv64` | Run ArceOS QEMU tests for riscv64 |
| `/test axvisor` | Run Axvisor QEMU tests for all 3 architectures |
| `/test starry all` | Run StarryOS QEMU tests for all 4 architectures |

## Implementation

Determine the command to run:

- `fmt`: `cargo fmt --all -- --check`
- `clippy`: `cargo xtask clippy`
- `quick`: `cargo fmt --all -- --check && cargo xtask clippy && cargo xtask sync-lint`
- `full`: all commands from `config/docker-ci.toml` [full] section
- `<os>` + `<arch>`: `cargo xtask <os> test qemu --arch <arch>`
- `<os>` + `all`: run the test for all architectures supported by that OS

Then execute:

```bash
.claude/scripts/local-ci.sh <mapped-command> <args>
```

If `local-ci.sh` returns non-zero, report the failures clearly with the failing command.

Save results to `.claude/cache/last-ci-result.json`:

```json
{
  "timestamp": "<ISO timestamp>",
  "status": "pass|fail",
  "command": "<what was run>",
  "results": [
    {"command": "cargo fmt --all -- --check", "pass": true, "output": "..."},
    {"command": "cargo xtask clippy", "pass": false, "output": "error: ..."}
  ]
}
```

For single-architecture tests dispatched to `<os> <arch>`, map to:
```bash
.claude/scripts/local-ci.sh test <os> <arch>
```
```

- [ ] **Step 2: Commit**

```bash
git add .claude/commands/test.md
git commit -m "feat(plugin): add /test command — Docker-based CI testing"
```

### Task 3.2: Create /pr-prep command

**Files:**
- Create: `.claude/commands/pr-prep.md`

- [ ] **Step 1: Write commands/pr-prep.md**

```markdown
---
name: pr-prep
description: Full PR workflow — clean branch, code, CI loop, review loop, create PR
args:
  - name: title
    type: string
    required: true
  - name: base
    type: string
    required: false
    default: upstream/dev
---

# /pr-prep — Complete PR preparation workflow

## Phase 1: Branch Setup

Execute:
```bash
git fetch upstream dev 2>/dev/null || git fetch origin dev
UPSTREAM_REF=$(git rev-parse upstream/dev 2>/dev/null || git rev-parse origin/dev)
# Sanitize title into branch name
BRANCH_NAME=$(echo "$ARGUMENTS_title" | tr ' ' '-' | tr -cd 'a-zA-Z0-9/-' | tr '[:upper:]' '[:lower:]')
git checkout -b "$BRANCH_NAME" "$UPSTREAM_REF"
```

If this fails (e.g., upstream remote not configured), tell the user:
> "Cannot find upstream/dev. Configure it with: `git remote add upstream https://github.com/rcore-os/tgoskits.git`"

Create the task tracking files:
```bash
echo "$BRANCH_NAME" > .claude/cache/task-active.flag
date -u +%Y-%m-%dT%H:%M:%SZ > .claude/cache/task-started-at.txt
```

## Phase 2: Coding

Tell the user: "Branch `$BRANCH_NAME` is ready. Start coding. The PostToolUse hook will automatically log changes to `log.md`. When you're done coding, say 'proceed to CI' or I'll continue automatically after a natural pause."

The AI writes code normally. The PostToolUse hook (Task 2.1) automatically appends to `log.md`.

## Phase 3: CI Loop

When the user indicates coding is complete (or after a significant coding round):

```bash
.claude/scripts/local-ci.sh full
```

**If CI passes:** → Phase 4

**If CI fails:**
1. Show the failing commands and their output
2. Analyze the failures
3. Fix the code
4. Re-run CI
5. Repeat up to **5 iterations**

After each fix iteration, summarize to the user:
> "CI iteration <N>/5: Fixed <what>. <X> checks still failing. Remaining failures: <list>"

If 5 iterations reached and CI still fails:
> "CI loop limit reached (5 iterations). Manual intervention needed. Failing: <list>"

Do NOT proceed to Phase 4 until CI passes.

## Phase 4: Self-Review Loop

Once CI passes, launch the PR-Review Agent (read `.claude/agents/pr-review.md` and follow its workflow).

**If review passes (no BLOCK items):** → Phase 5

**If review finds BLOCK items:**
1. Auto-fix the BLOCK items
2. Re-run `./scripts/local-ci.sh quick`
3. Re-review
4. Repeat up to **3 iterations**

After each review iteration, summarize:
> "Review iteration <N>/3: Fixed <X> BLOCK items, <Y> WARN items remaining."

If 3 iterations reached and BLOCK items remain:
> "Review loop limit reached (3 iterations). Remaining BLOCK items: <list>. Proceed anyway or wait for manual fix?"

## Phase 5: PR Creation

Generate PR body using this template:

```markdown
## Summary
<One-line summary of what this PR does>

### 1. <Issue Title>

**Type**: <behavior-bug|memory-bug|concurrency-bug|crash-bug|access-bug|resource-bug|missing-feature>

**Analysis**: <Root cause — which function/line, why it's wrong>

**Solution**: <What files were changed, what was done>

## Expected Behavior
- <Expected outcome 1>
- <Expected outcome 2>
```

Execute:
```bash
git push -u origin HEAD
gh pr create --base dev --title "$ARGUMENTS_title" --body "$PR_BODY"
```

Then generate the journal:
```bash
python3 .claude/scripts/journal-generator.py "$BRANCH_NAME"
```

Report the PR URL and journal path to the user.

## Error Handling

- If `gh` CLI is not installed: "Install GitHub CLI: https://cli.github.com/"
- If not authenticated: "Run `gh auth login` first"
- If push fails due to permissions: "You may not have push access to this repo. Fork it first."
```

- [ ] **Step 2: Commit**

```bash
git add .claude/commands/pr-prep.md
git commit -m "feat(plugin): add /pr-prep command — 5-phase PR workflow"
```

---

## Batch 4: Agents + Syscall Diff

### Task 4.1: Create syscall-diff.py — Linux vs OS behavior comparison

**Files:**
- Create: `.claude/scripts/syscall-diff.py`

- [ ] **Step 1: Write scripts/syscall-diff.py**

```python
#!/usr/bin/env python3
"""
Compare reference (Linux strace) syscall trace with target OS output.

Usage:
    python3 syscall-diff.py <linux-trace.log> <os-output.log> [--json]

Inputs:
    linux-trace.log: Output of `strace -f -v -o linux-trace.log <test-program>`
    os-output.log:   stdout + stderr + exit code from OS QEMU run

Output:
    Markdown diff report (or JSON with --json)
"""
import os
import re
import sys
import json
import difflib
from dataclasses import dataclass, field
from typing import Optional


@dataclass
class SyscallRow:
    pid: int
    name: str
    args: str
    result: str
    line_no: int


def parse_strace_log(path: str) -> list[SyscallRow]:
    """Parse strace -f -v output into structured syscall rows."""
    rows = []
    # Pattern: pid   syscall(args...) = result
    pattern = re.compile(
        r'^(\d+)\s+'
        r'(\w+(?:\([^)]*\))?)\('  # syscall name with optional (parens)
        r'(.*)'
        r'\)\s*=\s*(.*)$'
    )
    # Simpler fallback: pid   syscall(args) = result
    pattern2 = re.compile(
        r'^(\d+)\s+(\w+)\((.+)\)\s*=\s*(.+)$'
    )

    with open(path) as f:
        for i, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            # Skip strace headers (+++ exited with, --- SIG, etc.)
            if line.startswith(("+++", "---", "strace:")):
                continue

            m = pattern2.match(line)
            if not m:
                # Try to match incomplete lines (unfinished syscalls)
                m2 = re.match(r'^(\d+)\s+(\w+)\((.+)\s+<unfinished\s*\.\.\.>$', line)
                if m2:
                    rows.append(SyscallRow(
                        pid=int(m2.group(1)),
                        name=m2.group(2),
                        args=m2.group(3),
                        result="<unfinished>",
                        line_no=i
                    ))
                continue

            rows.append(SyscallRow(
                pid=int(m.group(1)),
                name=m.group(2),
                args=m.group(3),
                result=m.group(4),
                line_no=i
            ))
    return rows


def parse_os_output(path: str) -> dict:
    """Parse OS QEMU output log."""
    with open(path) as f:
        content = f.read()

    # Try to extract exit code
    exit_match = re.search(r'exit\s*code[:\s]*(\d+)', content, re.IGNORECASE)
    exit_code = int(exit_match.group(1)) if exit_match else None

    return {
        "stdout": content,
        "stderr": "",
        "exit_code": exit_code,
    }


def compare_syscall_lists(linux_rows: list[SyscallRow], os_output: dict) -> dict:
    """Compare syscall sequences between Linux and OS."""
    linux_syscalls = [(r.name, r.result) for r in linux_rows]
    linux_names = [r.name for r in linux_rows]

    # For OS output, try to find syscall-like patterns
    os_syscall_pattern = re.compile(
        r'(?:syscall|SYSCALL|sys_)?(\w+)\s*[=(]\s*([^,\n]+)',
        re.IGNORECASE
    )
    os_syscalls = []
    for m in os_syscall_pattern.finditer(os_output.get("stdout", "")):
        os_syscalls.append((m.group(1), m.group(2).strip()))

    issues = []
    if not os_syscalls:
        issues.append({
            "type": "warning",
            "msg": "Could not extract syscall trace from OS output. Comparing only final output.",
        })

    # Compare: check if Linux syscalls appear in OS output
    linux_set = set(linux_names)
    os_set = set(s[0] for s in os_syscalls) if os_syscalls else set()
    missing = linux_set - os_set
    extra = os_set - linux_set

    if missing:
        issues.append({
            "type": "missing_syscall",
            "syscalls": sorted(missing),
            "msg": f"OS missing {len(missing)} syscalls that Linux uses",
        })
    if extra:
        issues.append({
            "type": "extra_syscall",
            "syscalls": sorted(extra),
            "msg": f"OS uses {len(extra)} syscalls not present in Linux trace",
        })

    return {
        "linux_syscall_count": len(linux_rows),
        "os_syscall_count": len(os_syscalls),
        "issues": issues,
        "linux_syscalls": linux_syscalls,
        "os_syscalls": os_syscalls,
    }


def compare_output(linux_output: str, os_output: dict) -> dict:
    """Compare stdout/stderr/exit code."""
    issues = []

    os_stdout = os_output.get("stdout", "")
    linux_stdout = linux_output

    if os_stdout != linux_stdout:
        diff = list(difflib.unified_diff(
            linux_stdout.splitlines(keepends=True),
            os_stdout.splitlines(keepends=True),
            fromfile="linux-output",
            tofile="os-output",
            lineterm="",
        ))
        issues.append({
            "type": "output_mismatch",
            "diff": diff[:100],  # limit diff size
            "msg": "stdout/stderr differs between Linux and OS",
        })

    if os_output.get("exit_code") is not None:
        # Linux exit code would be in the strace log "+++ exited with X +++"
        pass  # handled by strace parsing

    return {"issues": issues, "match": len(issues) == 0}


def generate_report(linux_file: str, os_file: str, syscall_diff: dict, output_diff: dict) -> str:
    """Generate markdown diff report."""
    lines = [
        "# Syscall Behavior Diff Report",
        "",
        f"**Linux trace**: `{linux_file}`",
        f"**OS output**: `{os_file}`",
        "",
        "## Syscall Comparison",
        "",
        f"- Linux syscalls: {syscall_diff['linux_syscall_count']}",
        f"- OS syscalls detected: {syscall_diff['os_syscall_count']}",
        "",
    ]

    if syscall_diff["issues"]:
        for issue in syscall_diff["issues"]:
            lines.append(f"### {issue['type']}")
            lines.append(f"**{issue['msg']}**")
            if "syscalls" in issue:
                for s in issue["syscalls"]:
                    lines.append(f"- `{s}`")
            lines.append("")

    lines.append("## Output Comparison")
    lines.append("")
    if output_diff["match"]:
        lines.append("Output matches.")
    else:
        for issue in output_diff["issues"]:
            lines.append(f"### {issue['type']}")
            lines.append(f"**{issue['msg']}**")
            if "diff" in issue:
                lines.append("```diff")
                for d in issue["diff"]:
                    lines.append(d)
                lines.append("```")
            lines.append("")

    return "\n".join(lines)


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("Usage: syscall-diff.py <linux-trace.log> <os-output.log> [--json]")
        sys.exit(1)

    linux_file = sys.argv[1]
    os_file = sys.argv[2]
    as_json = "--json" in sys.argv

    linux_rows = parse_strace_log(linux_file)

    # Read Linux output from the strace log's process output
    # (strace -f output includes child process output interleaved)
    with open(linux_file) as f:
        linux_raw = f.read()
    # Extract lines that are NOT strace lines (process output)
    linux_output = "\n".join(
        line for line in linux_raw.split("\n")
        if not re.match(r'^\d+\s+\w+\(', line)
        and not line.startswith(("+++", "---", "strace:"))
    )

    os_output = parse_os_output(os_file)

    syscall_diff = compare_syscall_lists(linux_rows, os_output)
    output_diff = compare_output(linux_output, os_output)

    if as_json:
        result = {
            "syscall_diff": syscall_diff,
            "output_diff": output_diff,
        }
        print(json.dumps(result, indent=2))
    else:
        report = generate_report(linux_file, os_file, syscall_diff, output_diff)
        print(report)
```

- [ ] **Step 2: Test with sample strace output**

```bash
echo '12345 write(1, "hello\n", 6) = 6
12345 exit_group(0) = ?
+++ exited with 0 +++' > /tmp/test-linux.log
echo 'hello
exit code: 0' > /tmp/test-os.log
python3 .claude/scripts/syscall-diff.py /tmp/test-linux.log /tmp/test-os.log
```
Expected: Report showing 2 Linux syscalls, output comparison.

- [ ] **Step 3: Commit**

```bash
git add .claude/scripts/syscall-diff.py
git commit -m "feat(plugin): add syscall-diff.py — Linux vs OS behavior comparison"
```

### Task 4.2: Create Bug-Hunt Agent

**Files:**
- Create: `.claude/agents/bug-hunt.md`

- [ ] **Step 1: Write agents/bug-hunt.md**

```markdown
---
name: bug-hunt
description: Find bugs (behavior mismatches with Linux or unsafe code), write repro tests, fix, verify, and optionally create PR
skills:
  - starry-test-suit
  - cross-kernel-driver
  - arceos-test-adapter
tools:
  - Read
  - Write
  - Edit
  - Bash
  - Grep
  - Glob
---

# Bug-Hunt Agent

You are a kernel bug hunter. Your mission: find code whose behavior differs from standard Linux or that is unsafe, write a reproducible test case, fix the bug, verify the fix, and report your findings.

## Bug Classification

When you identify a potential bug, classify it:

| Type | Criteria | Example |
|------|----------|---------|
| **behavior-bug** | syscall return value, errno, or output differs from Linux | `timer_create` returns wrong errno |
| **crash-bug** | kernel panic, deadlock, infinite loop | NULL deref in signal handler |
| **memory-bug** | memory leak, use-after-free, double-free, buffer overflow | freeing struct then accessing its field |
| **concurrency-bug** | race condition, unsynchronized shared state | signal handler and timer callback race on same variable |
| **access-bug** | unchecked user pointer, missing capability/permission check | dereferencing user-space pointer directly |
| **resource-bug** | fd leak, integer overflow, resource exhaustion | timer counter overflow causes infinite wait |
| **missing-feature** | syscall or function entirely unimplemented | `timer_getoverrun` returns ENOSYS |

## Phase 1: HUNT (Discovery)

1. **Determine scope.** The user specifies what to investigate (syscall name, module, file path). If not specified, analyze recent changes from `log.md` or `git diff`.

2. **Run reference.** In the Docker container (`tgoskits-ci`), run the relevant test program under strace:
```bash
.claude/scripts/local-ci.sh ensure-base  # make sure image exists
docker run --rm -v "$PWD:/workspace" -w /workspace tgoskits-ci bash -c '
  strace -f -v -o /tmp/trace.log <test-program>
  cat /tmp/trace.log
'
```

3. **Run on target OS.** Run the same test on the target OS via QEMU:
```bash
docker run --rm -v "$PWD:/workspace" -w /workspace tgoskits-ci bash -c '
  cargo xtask <os> qemu --package <test> --arch <arch>
' > /tmp/os-output.log 2>&1
```

4. **Diff.** Run:
```bash
python3 .claude/scripts/syscall-diff.py /tmp/trace.log /tmp/os-output.log
```

5. **Report findings.** List all discrepancies found.

## Phase 2: REPRO (Reproduction)

For each confirmed discrepancy:

1. **Classify the bug** using the table above.
2. **Write a minimal test case** that triggers ONLY this bug:
   - C tests go in `test-suit/starryos/normal/<category>/c/src/main.c`
   - Create `CMakeLists.txt` for C tests
   - Add toml config files for each architecture
   - Mark with appropriate success/fail regexes
3. **Validate on Linux.** Run the test in the Docker container to get expected output:
```bash
docker run --rm -v "$PWD:/workspace" -w /workspace tgoskits-ci bash -c '
  cd <test-dir> && mkdir -p build && cd build && cmake .. && make
  ./test-program
'
```

## Phase 3: FIX

1. **Locate the source.** Find the exact file and function responsible.
2. **Apply the fix.** Make minimal changes — fix only the bug, no refactoring.
3. **Run the repro test** on the target OS. Confirm output matches Linux.

## Phase 4: VERIFY

1. Run quick CI to check for regressions:
```bash
.claude/scripts/local-ci.sh quick
```
2. If quick CI passes and time allows, run architecture-specific QEMU tests for affected architectures.

## Phase 5: REPORT

1. **Generate a commit** with message format: `fix(<scope>): <description>`
2. **If the user wants a PR**, follow the PR body template from `/pr-prep` Phase 5.
3. **Generate the journal** if this completes a task:
```bash
python3 .claude/scripts/journal-generator.py <task-name>
```

## Important Rules

- Always verify reference behavior against Linux in Docker before claiming a bug
- A bug is ONLY confirmed when: (a) behavior differs from Linux, OR (b) code is provably unsafe (memory bug, access bug)
- Write the minimal possible repro test — the shortest C program that triggers the bug
- Do not fix multiple unrelated bugs in one commit
- If you cannot reproduce the bug reliably, report it as "unconfirmed" and do not attempt a fix
```

- [ ] **Step 2: Commit**

```bash
git add .claude/agents/bug-hunt.md
git commit -m "feat(plugin): add bug-hunt agent — 5-phase bug discovery and fix"
```

### Task 4.3: Create PR-Review Agent

**Files:**
- Create: `.claude/agents/pr-review.md`

- [ ] **Step 1: Write agents/pr-review.md**

```markdown
---
name: pr-review
description: Review PR changes for POSIX/Linux semantic correctness, syscall consistency, safety, and code quality
skills:
  - review-open-prs
  - starry-test-suit
  - arceos-test-adapter
tools:
  - Read
  - Write
  - Edit
  - Bash
  - Grep
  - Glob
---

# PR-Review Agent

You are a kernel code reviewer. Review code changes against Linux/POSIX semantics and safety requirements.

## Review Dimensions

| Dimension | Check | Severity |
|-----------|-------|----------|
| **Syscall semantics** | Return values, errno match POSIX/Linux man-pages | BLOCK |
| **Boundary handling** | NULL, 0 length, negative offset, overflow inputs | BLOCK |
| **Resource leaks** | fd not closed, unfreed allocations, unlocked mutex | BLOCK |
| **Concurrency safety** | Race conditions on shared state | WARN |
| **Layer violation** | Kernel code calling ulib types directly | WARN |
| **Test coverage** | New syscall has corresponding test-suit case | INFO |

## Workflow

### Step 1: Get the diff

```bash
# For a branch:
git diff upstream/dev...HEAD

# Or for staged changes:
git diff --cached

# Or specify files manually
```

### Step 2: Per-file review

For each changed file:
1. Read the entire file (not just the diff) to understand context
2. For each modified function, check:
   - Does it match Linux behavior? (consult man-pages or Linux source if needed)
   - Are all error paths handled? (check every `return` for correct errno)
   - Are user-space pointers validated before dereference?
   - Are allocated resources freed on all paths?
   - Are locks properly acquired and released?
3. Check layer boundaries: kernel code should not directly use ulib types
4. Check if new functionality has corresponding test coverage

### Step 3: Generate REVIEW.md

```markdown
# REVIEW.md

**Branch**: <branch>
**Reviewed files**: <count>
**Date**: <date>

## BLOCK Items (must fix)

### <file>:<line> — <issue title>
**Severity**: BLOCK
**Dimension**: <syscall-semantics|boundary|resource-leak>
**Problem**: <description>
**Fix**: <suggested fix>

## WARN Items (should fix)

### <file>:<line> — <issue title>
...

## INFO Items (consider)

### <file>:<line> — <issue title>
...
```

### Step 4: Auto-fix BLOCK items

For each BLOCK item, apply the fix directly to the source file.

### Step 5: Re-verify

After fixing all BLOCK items:
```bash
.claude/scripts/local-ci.sh quick
```

If CI fails, fix and re-run. If BLOCK items remain after fix, re-review.

### Step 6: Loop control

Maximum 3 review-fix-ci iterations. Report status after each iteration:
> "Review iteration <N>/3: fixed <X> BLOCK, <Y> WARN remaining."

## Safety Checklist (run mentally for each function)

1. Is every user-provided pointer validated before use?
2. Is every allocation matched with a deallocation on all code paths?
3. Is every lock acquisition matched with a release?
4. Are array indices bounds-checked?
5. Are integer operations checked for overflow?
6. Can this code path be reached from interrupt context? If so, is it safe?
```

- [ ] **Step 2: Commit**

```bash
git add .claude/agents/pr-review.md
git commit -m "feat(plugin): add pr-review agent — semantic code review with auto-fix"
```

### Task 4.4: Create Test-Gen Agent

**Files:**
- Create: `.claude/agents/test-gen.md`

- [ ] **Step 1: Write agents/test-gen.md**

```markdown
---
name: test-gen
description: Generate test cases based on reference Linux behavior for syscall or system features
skills:
  - starry-test-suit
  - arceos-test-adapter
tools:
  - Read
  - Write
  - Bash
  - Grep
  - Glob
---

# Test-Gen Agent

You generate test cases for TGOSKits OS components. Every test must be validated against reference Linux behavior before being added.

## Input

- Target syscall or feature name (e.g., `timer_create`, `fallocate`)
- Or auto-triggered from Bug-Hunt / PR-Review agent output

## Workflow

### Step 1: Research Linux reference behavior

In the Docker container, write a C program that exercises the target syscall with all identified scenarios:

```bash
docker run --rm -v "$PWD:/workspace" -w /workspace tgoskits-ci bash -c '
  cat > /tmp/test.c << '\''EOF'\''
<C test program>
EOF
  gcc -o /tmp/test /tmp/test.c
  strace -f -v -o /tmp/trace.log /tmp/test
  echo "EXIT_CODE: $?"
  cat /tmp/trace.log
'
```

### Step 2: Coverage design

For each syscall, cover these scenarios:

| Scenario | Example (timer_create) |
|----------|------------------------|
| Normal path | Create CLOCK_REALTIME timer, set expiry, wait for signal |
| Invalid args — bad clock | CLOCK_TAI → EINVAL |
| Invalid args — bad flags | Invalid flag bits → EINVAL |
| Invalid args — NULL evp | NULL sigevent → EFAULT (if detectable) |
| Boundary — zero timeout | it_value = {0,0} |
| Boundary — very short | it_value = {0,1} (1ns) |
| Boundary — very long | it_value = {INT_MAX, 999999999} |
| Resource limit | Create many timers → EAGAIN at limit |
| Signal delivery | Verify siginfo_t content (si_signo, si_code, si_value) |
| Concurrency (if applicable) | Multiple threads creating/deleting timers |

### Step 3: Generate test files

**For C tests (starryos):**

```
test-suit/starryos/normal/<category>/<test-name>/
├── c/
│   ├── CMakeLists.txt
│   └── src/
│       └── main.c
├── qemu-aarch64.toml
├── qemu-riscv64.toml
├── qemu-x86_64.toml
└── qemu-loongarch64.toml
```

CMakeLists.txt template:
```cmake
cmake_minimum_required(VERSION 3.10)
project(test-<name> C)
set(CMAKE_C_STANDARD 11)
add_executable(test-<name> src/main.c)
```

qemu-arch.toml template:
```toml
[test]
name = "<test-name>"
type = "normal"
success_regex = "<expected output pattern>"
fail_regex = "<failure pattern>"
timeout = 30
```

**For Rust tests (arceos):**
```
test-suit/arceos/rust/<category>/
├── Cargo.toml
├── qemu-aarch64.toml
├── qemu-riscv64.toml
├── qemu-x86_64.toml
└── src/
    └── main.rs
```

### Step 4: Validate

1. Run on Linux (Docker): confirm expected output + exit code
2. Run on target OS (QEMU): confirm output matches Linux
3. If mismatch: report to user, suggest invoking Bug-Hunt Agent

### Step 5: Output

Report: list of created files, coverage summary, validation results.

If all tests pass on Linux and match on the target OS: "All tests validated. Ready to commit."
If some tests fail on the target OS: "X/Y tests fail on target OS. Consider running Bug-Hunt Agent on: <list>."
```

- [ ] **Step 2: Commit**

```bash
git add .claude/agents/test-gen.md
git commit -m "feat(plugin): add test-gen agent — Linux-reference test generation"
```

### Task 4.5: Create Driver-Audit Agent

**Files:**
- Create: `.claude/agents/driver-audit.md`

- [ ] **Step 1: Write agents/driver-audit.md**

```markdown
---
name: driver-audit
description: Audit driver code for correct layering (Driver Core / Capability Boundary / OS Glue / Runtime)
skills:
  - cross-kernel-driver
tools:
  - Read
  - Grep
  - Glob
---

# Driver-Audit Agent

You audit driver code under `drivers/` for correct architectural layering.

## The Four Layers

```
┌─ Driver Core ───────────────────────────────┐
│  Pure device logic.                          │
│  MUST: no OS-specific types or imports.      │
│  MUST: register access via mmio-api.         │
│  MUST NOT: raw pointer MMIO casts.           │
├─ Capability Boundary ────────────────────────┤
│  Trait interfaces to OS services.            │
│  MUST: IRQ via event contracts (no hardcoded │
│        interrupt numbers).                   │
│  MUST: DMA via dma-api.                      │
├─ OS Glue ────────────────────────────────────┤
│  Platform adaptation (axplat).               │
│  MUST: correct axplat dependency.            │
│  MUST: feature gates for platform selection. │
├─ Runtime ────────────────────────────────────┤
│  Initialization, registration with axdriver. │
│  MUST: proper devfs node creation.           │
│  MUST: clean error handling on init failure. │
└──────────────────────────────────────────────┘
```

## Workflow

### Step 1: Determine scope

The user specifies a driver directory or file. If not:
- Check `drivers/` for recently modified files: `git diff --name-only HEAD~1 -- drivers/`
- Or audit all drivers

### Step 2: Per-file audit

For each file in scope, check:

#### Driver Core checks (BLOCK)
1. Search for OS-specific imports:
```bash
grep -n 'use\s\+\(axhal\|axmm\|axtask\|axsync\|axdriver\|axfs\|axnet\|starry\)' <file>
```
If found → BLOCK: "OS module import in driver core"

2. Search for raw pointer MMIO:
```bash
grep -n '\(as\s\+\*mut\|as\s+\*const\).*\(0x\|addr\|base\)' <file>
```
If found AND not wrapped by mmio-api → BLOCK: "Raw pointer cast for MMIO, use mmio-api"

#### Capability Boundary checks (BLOCK)
3. Search for hardcoded interrupt numbers:
```bash
grep -n 'irq\s*=\s*[0-9]\|interrupt\s*=\s*[0-9]\|IRQ_[0-9]' <file>
```
If found → BLOCK: "Hardcoded interrupt number, use IRQ contract"

4. Check DMA operations use dma-api:
```bash
grep -n 'DMA\|dma\|Dma' <file>
```
If DMA operations found but no `use dma_api` import → BLOCK: "DMA operation without dma-api"

#### OS Glue checks (WARN)
5. Check axplat dependency is declared:
```bash
grep -n 'axplat\|ax-plat' <file>
```
If platform-specific code but no axplat reference → WARN: "Missing axplat dependency"

6. Check feature gates:
```bash
grep -n '#\[cfg(feature' <file> || true
```
If platform-conditional code without feature gate → WARN: "Missing feature gate for platform-specific code"

#### Runtime checks (INFO)
7. Check driver registration:
```bash
grep -n 'register\|init\|probe' <file>
```
If no registration found → INFO: "No driver registration call found"

### Step 3: Generate AUDIT.md

```markdown
# AUDIT.md

**Scope**: <directory or files>
**Date**: <date>

## BLOCK Items

### <file>:<line> — <violation>
**Layer**: <driver-core|capability-boundary>
**Problem**: <description>
**Fix**: <suggestion>

## WARN Items
...

## INFO Items
...
```

### Step 4: Report

Present the audit findings to the user. Do NOT auto-fix driver code unless the user explicitly asks — driver changes require hardware testing.
```

- [ ] **Step 2: Commit**

```bash
git add .claude/agents/driver-audit.md
git commit -m "feat(plugin): add driver-audit agent — 4-layer driver code audit"
```

### Task 4.6: Final verification — validate complete plugin structure

**Files:**
- None (verification only)

- [ ] **Step 1: Verify complete file tree**

```bash
find .claude -type f | sort
```
Expected output:
```
.claude/agents/bug-hunt.md
.claude/agents/driver-audit.md
.claude/agents/pr-review.md
.claude/agents/test-gen.md
.claude/cache/.gitkeep
.claude/commands/pr-prep.md
.claude/commands/test.md
.claude/config/docker-ci.toml
.claude/hooks/pre-pr-gate.md
.claude/hooks/session-end-journal.md
.claude/plugin.json
.claude/scripts/journal-generator.py
.claude/scripts/local-ci.sh
.claude/scripts/post-tool-use-log.py
.claude/scripts/syscall-diff.py
.claude/settings.json
.claude/skills/arceos-test-adapter/SKILL.md
.claude/skills/board-uboot-fsck-repair/...
.claude/skills/cross-kernel-driver/...
.claude/skills/review-open-prs/...
.claude/skills/starry-test-suit/...
.claude/skills/update-std-tests/...
```

- [ ] **Step 2: Validate plugin.json is parseable JSON**

```bash
python3 -c "import json; json.load(open('.claude/plugin.json')); print('plugin.json: OK')"
```
Expected: `plugin.json: OK`

- [ ] **Step 3: Validate settings.json is parseable JSON**

```bash
python3 -c "import json; json.load(open('.claude/settings.json')); print('settings.json: OK')"
```
Expected: `settings.json: OK`

- [ ] **Step 4: Validate Python scripts have no syntax errors**

```bash
python3 -m py_compile .claude/scripts/syscall-diff.py && echo "syscall-diff.py: OK"
python3 -m py_compile .claude/scripts/journal-generator.py && echo "journal-generator.py: OK"
python3 -m py_compile .claude/scripts/post-tool-use-log.py && echo "post-tool-use-log.py: OK"
```
Expected: All OK.

- [ ] **Step 5: Validate bash script syntax**

```bash
bash -n .claude/scripts/local-ci.sh && echo "local-ci.sh: OK"
```
Expected: `local-ci.sh: OK`

- [ ] **Step 6: Commit verification results**

No file changes needed; verification is complete.
```

---

## Implementation Order Summary

| Batch | Tasks | Dependencies |
|-------|-------|-------------|
| **Batch 1** | 1.1–1.5 (Foundation + Docker CI) | None |
| **Batch 2** | 2.1–2.5 (Hooks + Journal) | Batch 1 |
| **Batch 3** | 3.1–3.2 (Commands) | Batch 1, 2 |
| **Batch 4** | 4.1–4.6 (Agents + Syscall Diff) | Batch 1, 2 |

Each batch produces working, independently testable functionality.
