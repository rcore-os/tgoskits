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
    if not entries:
        print(f"No log entries found in {LOG_PATH} — skipping journal generation")
        sys.exit(0)
    journal = generate_journal(task_name, entries)
    output_path = os.path.join(WORKSPACE, f"{task_name}-journal.md")
    with open(output_path, "w") as f:
        f.write(journal)
    print(f"Journal written to {output_path}")

    # Clear consumed entries from log.md
    if os.path.exists(LOG_PATH):
        os.remove(LOG_PATH)
        print(f"Cleared {LOG_PATH} (entries archived to journal)")
