---
name: reassign-pr-reviewers
description: Use when the user wants to assign or rebalance GitHub pull request reviewers in rcore-os/tgoskits from a discussion, ownership matrix, open PR scope, or existing reviewer requests, especially when preserving bot requests or handling reviewer permission limits matters.
---

# Reassign PR Reviewers

## Goal

Update reviewer requests for open PRs according to the repository reviewer-routing source of truth. Always use `.github/MAINTAINERS.md` as the strict reviewer allowlist: each section maps one or more GitHub reviewer logins from `R:` lines to path hints from `F:` lines and keyword/direction hints from `K:` lines. Treat reviewer assignment as a GitHub metadata operation: no code edits, no build/test validation, and no PR review submission.

## Source Of Truth

1. Resolve repository and user identity:
   ```bash
   gh auth status
   gh repo view --json nameWithOwner,defaultBranchRef,url
   ```
2. Read `.github/MAINTAINERS.md` from the target branch or current worktree before assigning reviewers. It is the local source of truth for PR reviewer routing.
3. Parse each maintainer section as an ownership rule:
   - `R:` is the reviewer login to request on GitHub. Use reviewer logins, not maintainer-only `M:` lines, unless the same login is also listed on `R:`.
   - `F:` is a path or glob hint. Match it against changed PR files.
   - `K:` is a comma-separated keyword or direction hint. Match these against the PR title, body, changed file paths, crate names, feature names, config names, and obvious filenames from the diff.
4. Build the allowed human reviewer set only from `R:` lines in `.github/MAINTAINERS.md`. Never request, retain as an ownership target, or infer a human target reviewer outside this allowed set. If another source names a human reviewer that is not present on an `R:` line, report it as ignored.
5. Prefer explicit keyword matches from `K:` when routing ambiguous PRs. Path matches from `F:` are also valid and should be used when the changed files clearly fall under a maintainer section.
6. If multiple maintainer sections match by keyword or path, request all matched `R:` reviewers after dropping the PR author. Do not collapse to a single reviewer unless the source of truth or user request says to do so.
7. If no `K:` or `F:` entry clearly matches a non-draft PR, default the target reviewer to `ZR233`, after confirming `ZR233` is present on an `R:` line. Report this as a fallback assignment, not as ownership evidence.
8. Skip draft PRs entirely. Do not add or remove reviewers on draft PRs unless the user explicitly says to include drafts.

An external assignment source, such as a GitHub discussion or concrete PR assignment table, may map PRs to ownership areas only when the user explicitly asks for it. It must not expand the reviewer allowlist beyond `.github/MAINTAINERS.md` `R:` lines. Read that source directly. For a discussion:
   ```bash
   gh api graphql \
     -f query='query($owner:String!,$repo:String!,$number:Int!){ repository(owner:$owner,name:$repo){ discussion(number:$number){ title body url author{login} comments(first:100){nodes{author{login} body createdAt}} } } }' \
     -F owner=<owner> -F repo=<repo> -F number=<discussion>
   ```

When an external source is used:

- Prefer explicit per-PR assignments from the source.
- For open PRs not listed there, infer reviewers from `.github/MAINTAINERS.md` using `K:` keywords first and `F:` paths second.
- Ignore reviewers that are not listed on an `R:` line in `.github/MAINTAINERS.md`; do not request them even if an external source names them.

## Gather Current State

List all open PRs and their current reviewer requests:

```bash
gh pr list --repo <owner>/<repo> --state open --limit 200 \
  --json number,title,body,author,isDraft,reviewRequests,files,updatedAt,url
```

Use the REST endpoint for exact requested reviewer state when applying changes:

```bash
gh api repos/<owner>/<repo>/pulls/<pr>/requested_reviewers
```

Important: preserving existing reviewer requests is mandatory by default unless the user explicitly asks to remove or rebalance reviewers. Existing human reviewer requests may have been assigned manually by an administrator, including reviewers outside `.github/MAINTAINERS.md`; carry them forward as preserved reviewers and never remove them in the default add-only flow. Existing bot review requests are also mandatory to preserve unless the user explicitly says to change bot requests.

Important: skip draft PRs entirely unless the user explicitly asks to include drafts. Record them in the dry run as skipped with reason `draft`.

## Build A Dry Run

Before writing to GitHub, produce a dry-run table with:

- PR number and author
- current requested reviewers
- target reviewers
- preserved existing reviewers
- preserved bot reviewers
- reviewers to remove
- reviewers to add
- matched maintainer section, keyword(s), and/or path hint(s)
- fallback reviewer, if no maintainer section matched
- skipped reason, if any

Always drop the PR author from the target reviewer list because GitHub cannot request a review from the author.

When computing add/remove operations, first compute ownership targets from `.github/MAINTAINERS.md`, then union existing reviewer requests into the final desired reviewer state. The default flow is add-only: `reviewers to remove` must be empty unless the user explicitly asks to remove or rebalance reviewer requests. If the user does request removals, existing bot reviewers still must stay requested unless the user explicitly says to change bot requests.

When using `.github/MAINTAINERS.md`, state the routing evidence for every target reviewer:

- `K:` evidence: the specific keyword(s) matched and where they appeared, such as title, body, file path, crate/config name, or diff-visible identifier.
- `F:` evidence: the specific path/glob hint and changed file(s) that matched.
- no evidence on a non-draft PR: target `ZR233` as the default fallback reviewer and state that no `K:`/`F:` ownership evidence matched.
- draft PR: intentionally skip and do not compute add/remove operations.

If a discussion contains both an old concrete assignment table and a broader ownership matrix, say which rule is being used for each PR group:

- listed PRs: use the concrete assignment table
- newer unlisted PRs: infer from `.github/MAINTAINERS.md` `K:` keywords and `F:` paths

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

In the default add-only flow, do not call the DELETE endpoint because existing reviewer requests must be preserved. If the user explicitly requested removals or a rebalance, apply allowed removals before additions for each PR, while still preserving bot reviewers unless bot removal was explicitly requested. Continue across independent PRs, but record each failed operation with the PR number, requested login, and GitHub error.

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
- PRs intentionally skipped, especially draft PRs, bot-authored PRs, or preserved existing reviewer requests
- PRs partially assigned because a target reviewer could not be requested
- exact blocked reviewer login and GitHub permission/API reason

State explicitly that no build or clippy validation was run when only GitHub reviewer metadata changed.
