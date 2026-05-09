#!/usr/bin/env python3
"""PostToolUse hook: append activity summary to log.md."""
import os
import sys
from datetime import datetime, timezone

PLUGIN_ROOT = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WORKSPACE = os.path.dirname(PLUGIN_ROOT)
LOG_PATH = os.path.join(WORKSPACE, "log.md")


def get_changed_files():
    """Get list of files modified in working tree (staged + unstaged vs HEAD)."""
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
            files.update(f.strip() for f in result.stdout.strip().split("\n") if f.strip())
        if staged.stdout:
            files.update(f.strip() for f in staged.stdout.strip().split("\n") if f.strip())
        return sorted(files)
    except Exception:
        return []


def get_last_commit_message():
    """Get the last commit's subject line."""
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
    summary = summary[:500]

    entry = f"""## {timestamp} — {count} file{'s' if count != 1 else ''} changed

**Files**: {file_list}

**Summary**: {summary}

---
"""
    # Deduplicate: don't write if last entry has the same content
    if os.path.exists(LOG_PATH):
        with open(LOG_PATH) as f:
            existing = f.read()
        if entry.strip() in existing:
            return

    with open(LOG_PATH, "a") as f:
        f.write(entry)


if __name__ == "__main__":
    files = get_changed_files()
    if not files:
        sys.exit(0)

    commit_msg = get_last_commit_message()
    summary = commit_msg if commit_msg else "Code changes (see git log for details)"
    append_log(files, summary)
