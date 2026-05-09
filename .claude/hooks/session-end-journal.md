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

Run the journal generator script:
```bash
python3 .claude/scripts/journal-generator.py "$(cat .claude/cache/task-active.flag)"
```

Or write `[task-name]-journal.md` manually with this structure:

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
