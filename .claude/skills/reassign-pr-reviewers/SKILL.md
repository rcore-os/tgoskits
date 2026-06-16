---
name: reassign-pr-reviewers
description: Use when the user wants to assign or rebalance GitHub pull request reviewers in rcore-os/tgoskits from a discussion, ownership matrix, open PR scope, or existing reviewer requests, especially when preserving bot requests or handling reviewer permission limits matters.
---

# Reassign PR Reviewers

## Goal

Update reviewer requests for open PRs according to a project-facing source of truth, such as a GitHub discussion that contains reviewer ownership areas or a concrete PR assignment table. Treat reviewer assignment as a GitHub metadata operation: no code edits, no build/test validation, and no PR review submission.

## Source Of Truth

1. Resolve repository and user identity:
   ```bash
   gh auth status
   gh repo view --json nameWithOwner,defaultBranchRef,url
   ```
2. Read the assignment source directly. For a discussion:
   ```bash
   gh api graphql \
     -f query='query($owner:String!,$repo:String!,$number:Int!){ repository(owner:$owner,name:$repo){ discussion(number:$number){ title body url author{login} comments(first:100){nodes{author{login} body createdAt}} } } }' \
     -F owner=<owner> -F repo=<repo> -F number=<discussion>
   ```
3. Prefer explicit per-PR assignments from the source. For open PRs not listed there, infer reviewers from the ownership matrix and the PR's title/files.
4. Skip or ask before assigning reviewers that are not named in the source or inferable from a clear ownership area.

## Gather Current State

List all open PRs and their current reviewer requests:

```bash
gh pr list --repo <owner>/<repo> --state open --limit 200 \
  --json number,title,author,isDraft,reviewRequests,files,updatedAt,url
```

Use the REST endpoint for exact requested reviewer state when applying changes:

```bash
gh api repos/<owner>/<repo>/pulls/<pr>/requested_reviewers
```

Important: preserve existing bot review requests unless the user explicitly says to change them. Bot-authored PRs, such as release automation PRs, should normally keep their existing reviewer state.

## Build A Dry Run

Before writing to GitHub, produce a dry-run table with:

- PR number and author
- current requested reviewers
- target reviewers
- reviewers to remove
- reviewers to add
- skipped reason, if any

Always drop the PR author from the target reviewer list because GitHub cannot request a review from the author.

If a discussion contains both an old concrete assignment table and a broader ownership matrix, say which rule is being used for each PR group:

- listed PRs: use the concrete assignment table
- newer unlisted PRs: infer from ownership matrix plus files/title

## Apply Changes

Do not rely on `gh pr edit --add-reviewer/--remove-reviewer` in this repository; it can fail because it queries deprecated Projects classic fields unrelated to reviewer assignment.

Use the pull request requested reviewers REST API instead:

```bash
# Add reviewers.
printf '%s\n' '{"reviewers":["<login>"]}' |
  gh api -X POST repos/<owner>/<repo>/pulls/<pr>/requested_reviewers --input -

# Remove reviewers.
printf '%s\n' '{"reviewers":["<login>"]}' |
  gh api -X DELETE repos/<owner>/<repo>/pulls/<pr>/requested_reviewers --input -
```

Apply removals before additions for each PR. Continue across independent PRs, but record each failed operation with the PR number, requested login, and GitHub error.

## Permission Handling

Reviewer requests may fail when the target user is not a collaborator or lacks sufficient repository permission. Check permissions when a request fails:

```bash
gh api repos/<owner>/<repo>/collaborators/<login>/permission --jq '.permission'
```

If GitHub rejects a reviewer:

- keep any successfully assigned reviewers on that PR
- add other requested reviewers that GitHub accepts
- report the missing reviewer and the observed permission or API error
- do not replace them with an unrelated reviewer unless the source of truth supports that fallback

## Final Verification

Read all open PR reviewer requests after applying changes:

```bash
gh pr list --repo <owner>/<repo> --state open --limit 200 \
  --json number,author,reviewRequests |
  jq -r 'sort_by(.number)[] | "#\(.number)\t\(.author.login)\t\(.reviewRequests|map(.login // .name // .slug // .__typename)|join(","))"'
```

Compare final state against the target map. The final response should include:

- count of open PRs considered
- PRs changed successfully
- PRs already matching target
- PRs intentionally skipped, especially bot-authored PRs or existing bot requests
- PRs partially assigned because a target reviewer could not be requested
- exact blocked reviewer login and GitHub permission/API reason

State explicitly that no build or clippy validation was run when only GitHub reviewer metadata changed.
