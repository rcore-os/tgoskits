#!/usr/bin/env python3
"""Stop hook: generate journal if task-active.flag exists, otherwise exit silently."""
import os
import subprocess
import sys

PLUGIN_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CACHE_DIR = os.path.join(PLUGIN_ROOT, "cache")
FLAG_FILE = os.path.join(CACHE_DIR, "task-active.flag")
STARTED_FILE = os.path.join(CACHE_DIR, "task-started-at.txt")

if not os.path.exists(FLAG_FILE):
    sys.exit(0)

# Flag exists — read task name
with open(FLAG_FILE) as f:
    task_name = f.read().strip()

if not task_name:
    sys.exit(0)

# Check if journal already exists — don't regenerate
workspace = os.path.dirname(PLUGIN_ROOT)
journal_path = os.path.join(workspace, f"{task_name}-journal.md")
if os.path.exists(journal_path):
    os.remove(FLAG_FILE)
    if os.path.exists(STARTED_FILE):
        os.remove(STARTED_FILE)
    sys.exit(0)

# Generate journal
generator = os.path.join(PLUGIN_ROOT, "scripts", "journal-generator.py")
result = subprocess.run([sys.executable, generator, task_name], cwd=workspace)

if result.returncode != 0:
    print(f"Journal generator failed with exit code {result.returncode}", file=sys.stderr)
    sys.exit(0)  # Don't cleanup flag — allow retry next session

# Cleanup only on success
os.remove(FLAG_FILE)
if os.path.exists(STARTED_FILE):
    os.remove(STARTED_FILE)
