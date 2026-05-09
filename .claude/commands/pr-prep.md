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

Establish a clean feature branch from upstream/dev.

Execute:
```bash
git fetch upstream dev 2>/dev/null || git fetch origin dev
UPSTREAM_REF=$(git rev-parse upstream/dev 2>/dev/null || git rev-parse origin/dev)
# Sanitize title into branch name
BRANCH_NAME=$(echo "$ARGUMENTS_title" | tr ' ' '-' | tr -cd 'a-zA-Z0-9/-' | tr '[:upper:]' '[:lower:]')
git checkout -b "$BRANCH_NAME" "$UPSTREAM_REF"
```

If upstream is not configured, tell the user:
> "Cannot find upstream/dev. Configure it with: `git remote add upstream https://github.com/rcore-os/tgoskits.git`"

Create task tracking files:
```bash
echo "$BRANCH_NAME" > .claude/cache/task-active.flag
date -u +%Y-%m-%dT%H:%M:%SZ > .claude/cache/task-started-at.txt
```

Report: "Branch `$BRANCH_NAME` created from upstream/dev. Ready for coding."

## Phase 2: Coding

Tell the user: "Branch is ready. Start coding — the hook will automatically log changes to `log.md`. When done, say 'proceed to CI'."

The AI writes code normally. The PostToolUse hook (configured in settings.json) automatically logs changes.

## Phase 3: CI Loop

When the user indicates coding is done:

```bash
bash .claude/scripts/local-ci.sh full
```

**If CI passes:** move to Phase 4.

**If CI fails:**
1. Show failing commands with their output
2. Analyze failures and fix the code
3. Re-run CI
4. Repeat up to **5 iterations**

After each fix, summarize:
> "CI iteration <N>/5: Fixed <what>. <X> checks still failing: <list>"

At 5 iterations with failures:
> "CI loop limit reached (5 iterations). Manual intervention needed."

Do NOT proceed to Phase 4 until CI passes.

## Phase 4: Self-Review Loop

Once CI passes, launch the PR-Review Agent. Read `.claude/agents/pr-review.md` if it exists and follow its workflow. If the agent file does not exist yet (it's being created in Batch 4), perform a manual review:

1. Review each changed file for:
   - Syscall semantics: do return values and errno match POSIX/Linux?
   - Boundary handling: are NULL, zero, negative, overflow inputs handled?
   - Resource leaks: are fds closed, memory freed, locks released?
   - Unsafe code: are user pointers validated, capabilities checked?
2. Fix any BLOCK-level issues
3. Re-run quick CI: `bash .claude/scripts/local-ci.sh quick`
4. Repeat up to **3 iterations**

After each iteration:
> "Review iteration <N>/3: Fixed <X> BLOCK, <Y> WARN items remaining."

At 3 iterations with remaining BLOCK items:
> "Review loop limit reached. Remaining BLOCK items: <list>. Proceed anyway?"

## Phase 5: PR Creation

Generate the PR body using this template:

```markdown
## Summary
<One-line summary of what this PR does>

### 1. <Issue Title>

**Type**: <behavior-bug | memory-bug | concurrency-bug | crash-bug | access-bug | resource-bug | missing-feature>

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

Report the PR URL and journal path.

## Error Handling

- If `gh` CLI is not installed: tell user to install from https://cli.github.com/
- If not authenticated: tell user to run `gh auth login`
- If push fails due to permissions: user may need to fork the repo
