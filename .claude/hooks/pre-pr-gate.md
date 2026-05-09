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

### Check 3: Direct Push (block for main/dev)

If the command is `git push` and the target branch is `main` or `dev`:

**BLOCK the command.** Tell the user:
> "Direct push to main/dev is forbidden. Use a feature branch and create a PR."

## If All Checks Pass

Allow the command to execute.
