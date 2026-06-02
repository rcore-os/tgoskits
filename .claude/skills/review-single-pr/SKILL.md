---
name: review-single-pr
description: Review one specified GitHub pull request in this tgoskits repository. Use when the user names a PR number or URL and asks to review, re-review, compare with Linux/POSIX/RFC/VirtIO semantics, check duplicate functionality or related open PRs, validate Starry app-support test placement, repair safe merge conflicts, run focused validation, leave Chinese inline review comments, approve, request changes, or assign reviewers after review.
---

# Review Single PR

## Goal

Perform a focused review of exactly one PR, using an isolated worktree and local validation before submitting a GitHub review. The review must also decide whether the PR duplicates existing base-branch functionality or overlaps with other open PRs. After the review decision is submitted, assign suitable human reviewers from the project reviewer direction table when the PR still needs domain follow-up. The normal outcome is either `APPROVE` when no blocking issue remains, or `REQUEST_CHANGES` with Chinese inline comments when the PR has correctness, standards, duplication, test, or CI coverage problems.

This skill is the authoritative single-PR workflow used by `review-open-prs`: do not fully review all open PRs, but always inspect enough related open PR context to classify duplicate, overlapping, superseded, or conflicting work.

## System Skill Priority

For GitHub operations, follow the system GitHub plugin skills first:

- Use `github:github` as the default source for repository orientation, PR metadata, patch inspection, comments, labels, reactions, and connector-first behavior.
- Use `github:gh-address-comments` when unresolved review threads, requested changes, inline review context, line anchors, or thread resolution state matter.
- Use `github:gh-fix-ci` when the review depends on failing GitHub Actions checks or logs.

Prefer the GitHub MCP/connector for structured PR data. Use local `git` for fetch, detached worktrees, local diffs, and validation. Use `gh` only for connector gaps such as current-branch PR discovery, GraphQL review-thread state, Actions logs, or review submission when the connector cannot preserve the required inline review anchors.

## Intake

1. Follow `github:github` to resolve repository identity, current user, PR number or URL, title, body, author, base/head refs, `headRefOid`, draft state, merge state, changed files, patch context, commit messages, existing reviews/comments, and available checks.
2. If the PR is authored by the current GitHub user, say so and ask before submitting a formal review.
3. Include draft PRs unless the user explicitly says to skip drafts.
4. Keep connector state and local checkout state aligned before creating the worktree.

Fallback only when the GitHub MCP/connector cannot provide the needed data:

   ```bash
   gh auth status
   gh repo view --json nameWithOwner,defaultBranchRef,url
   gh pr view <pr> --json number,title,body,author,baseRefName,headRefName,headRefOid,headRepositoryOwner,isDraft,mergeStateStatus,maintainerCanModify,reviewDecision,url,commits
   gh pr diff <pr> --patch --color=never
   gh pr checks <pr> --watch=false
   gh api "repos/<owner>/<repo>/pulls/<pr>/reviews?per_page=100"
   gh api "repos/<owner>/<repo>/pulls/<pr>/files?per_page=100"
   ```

## Review Threads And CI

For prior requested changes, unresolved review conversations, inline review locations, or resolution state, follow `github:gh-address-comments`: use the GitHub app/MCP for PR metadata and patch context, and use its GraphQL-based `gh` fallback only when thread-level fields such as `isResolved`, `isOutdated`, `diffSide`, or exact line anchors are required. Do not treat flat connector comments as a complete representation of review-thread state.

When using GraphQL directly, request `reviewThreads { nodes { id isResolved isOutdated path line diffSide comments(first: 100) { nodes { author { login } body createdAt } } } }`. Detached worktrees cannot rely on current-branch PR inference, so pass `<owner>`, `<repo>`, and `<pr>` explicitly to helpers.

Always inspect unresolved review conversations from previous reviews. If the concrete issue is fixed in the current PR head, resolve the conversation before finishing the review. Keep threads open when the fix is partial, the test is not wired into the runner, or the comment is still behaviorally valid. Resolving old threads does not imply approval if new blocking issues remain.

Resolve fixed conversations with the review-thread API, then fetch threads again and confirm every resolved thread reports `isResolved=true`:

```bash
gh api graphql \
  -f query='mutation($threadId:ID!){resolveReviewThread(input:{threadId:$threadId}){thread{id isResolved}}}' \
  -f threadId=<thread-id>
```

For failing, cancelled, missing, or suspicious GitHub Actions checks, follow `github:gh-fix-ci`: use the GitHub app/MCP for PR context and use `gh` for Actions check/log inspection because the connector does not expose that workflow end to end. Remote CI is evidence, not a substitute for local review and targeted validation.

Always inspect CI failures before submitting the review:

1. Fetch check summaries and enough logs to classify each non-passing required check:
   ```bash
   gh pr checks <pr> --repo <owner>/<repo> --watch=false
   gh run view <run-id> --repo <owner>/<repo> --log-failed
   ```
2. Decide whether each CI failure is caused by this PR's changed surface, a likely unrelated pre-existing/infrastructure failure, or unclear.
3. Treat CI as PR-related when the failing job exercises files, crates, cases, commands, platforms, or behavior changed by the PR; when the failure reproduces locally on the PR head but not on base; or when the new/modified tests, configs, or workflow steps cause the failure, hang, skip, or timeout.
4. Treat CI as unrelated only when there is concrete evidence: the failure is outside the changed surface, known flaky/infrastructure behavior, already fails on base, or is tracked by an existing issue. Do not mark a failure unrelated merely because local focused validation passed.
5. For unrelated CI failures, state that in the review body with the failing check name, observed failure, and why it is unrelated to this PR. Search for an existing issue before finishing:
   ```bash
   gh issue list --repo <owner>/<repo> --state open --search '<job name or distinctive error>'
   ```
   If no suitable open issue exists, create one with a neutral title and body describing the CI job, PR where it was observed, representative log excerpt, why it appears unrelated, and any reproduction or rerun evidence:
   ```bash
   gh issue create --repo <owner>/<repo> --title '<neutral CI issue title>' --body-file issue.md
   ```
   Link the existing or newly-created issue in the review body. Do not create duplicate issues.
6. For PR-related CI failures, submit `REQUEST_CHANGES`. The review body and any inline comment must explain the failing check, the concrete failure mode, why it belongs to this PR, and the expected fix direction.
7. When causality is unclear after reasonable log inspection, do not approve on CI alone. Either request changes with the concrete uncertainty and next debugging direction, or mark the review blocked/no-submit if the user asked for investigation only.

## Worktree

Fetch the PR and base, then review in a detached worktree:

```bash
repo_root="$(git rev-parse --show-toplevel)"
repo_parent="$(dirname "$repo_root")"
review_wt="$repo_parent/$(basename "$repo_root")-review-pr<pr>"
git fetch origin '+refs/pull/<pr>/head:refs/remotes/origin/pr/<pr>' '+refs/heads/*:refs/remotes/origin/*'
git worktree add --detach "$review_wt" origin/pr/<pr>
```

If the worktree already exists, reuse it only when clean and at the current PR head:

```bash
git -C "$review_wt" status --short
git -C "$review_wt" rev-parse HEAD
git rev-parse refs/remotes/origin/pr/<pr>
```

If it is stale and clean, update it non-destructively to the fetched PR head. If it has local changes, create a fresh worktree path or ask how to proceed. Do not modify or revert the user's main worktree while reviewing.

Never review multiple StarryOS QEMU cases in the same checkout at the same time. Use separate worktrees for parallel PR review.

## Merge Conflicts

Handle merge conflicts in either of these cases: the user explicitly asks for conflict handling, or this review has no blocking findings and would otherwise be `APPROVE` while the current PR metadata says `mergeStateStatus=DIRTY` and `maintainerCanModify=true`. Do not submit or reaffirm approval before the conflict repair is pushed and re-validated against the new head.

First refresh and classify the conflict and approval state:

```bash
gh pr view <pr> --json number,baseRefName,headRefName,headRepositoryOwner,headRefOid,mergeStateStatus,maintainerCanModify,reviewDecision,reviews
gh api "repos/<owner>/<repo>/pulls/<pr>/reviews?per_page=100"
```

- `reviewDecision=APPROVED` is the current aggregate approval state. Historical `APPROVED` review records are useful context, but do not by themselves mean the PR is currently approved; if aggregate approval is empty, `CHANGES_REQUESTED`, or review threads remain unresolved, treat conflict repair as a no-submit dry run unless the user specifically asked to push a repair.
- If `mergeStateStatus=UNKNOWN`, refresh or wait and query again before acting. Do not infer a current conflict from stale search results.
- If `mergeStateStatus=DIRTY` and `maintainerCanModify=false`, do not repair the branch. When conflict handling was explicitly requested, submit `REQUEST_CHANGES` explaining that the branch conflicts with base and maintainers cannot push a fix; ask the author to merge/rebase latest base and suggest enabling "Allow edits by maintainers". Otherwise, include the conflict limitation in the review body or user summary.
- If `mergeStateStatus=DIRTY` and `maintainerCanModify=true`, create a separate conflict worktree and verify the contributor branch still matches `headRefOid` before doing pushable work:

  ```bash
  conflict_wt="$repo_parent/$(basename "$repo_root")-conflict-pr<pr>"
  git fetch origin '+refs/pull/<pr>/head:refs/remotes/origin/pr/<pr>' '+refs/heads/<base>:refs/remotes/origin/<base>'
  git ls-remote "https://github.com/<head-owner>/<repo>.git" "refs/heads/<headRefName>"
  git worktree add --detach "$conflict_wt" origin/pr/<pr>
  git -C "$conflict_wt" merge --no-ff --no-commit "origin/<base>"
  git -C "$conflict_wt" diff --name-only --diff-filter=U
  ```

- In this detached conflict worktree, conflict marker `HEAD` / stage 2 / "ours" is the PR branch, and `origin/<base>` / stage 3 / "theirs" is current base. Use `git show :1:<path>`, `git show :2:<path>`, and `git show :3:<path>` when the ancestor, PR side, or base side is unclear.
- Resolve conflicts semantically according to PR intent and current base behavior. Do not merely keep both sides, and do not resurrect APIs or layouts that base already replaced. Port the PR feature onto the new base abstraction, then keep independent additions from both sides when they do not conflict.
- PR 837 is the reference example for this rule: the PR added `/proc/kallsyms`, while base had replaced the old `SeqFile` pattern with `SeqObject` plus `SpecialFsFile::new_regular_with_perm`. The correct repair was to keep the kallsyms feature but express it with the current base API, while also keeping independent base/PR additions such as `ktracepoint` plus `ksym` and `.tracepoint` plus `.kallsyms`.
- Before committing the repair, run formatting, marker checks, diff hygiene, and focused validation for the changed surface:

  ```bash
  cargo fmt
  rg -n '<<<<<<<|=======|>>>>>>>' <conflicted-files>
  git -C "$conflict_wt" diff --check
  <targeted cargo xtask/cargo test/cargo clippy commands>
  git -C "$conflict_wt" add <resolved-files>
  git -C "$conflict_wt" commit
  ```

- Before pushing a repaired conflict branch, refresh the PR and confirm the local merge commit's first parent equals the current remote `headRefOid`; also re-check the fork branch with `git ls-remote`. If the remote head changed, stop and re-review instead of pushing.
- Push repaired fork branches with a normal non-force push to the PR head owner and branch, for example `git push https://github.com/<head-owner>/<repo>.git HEAD:<headRefName>`. Never force-push a contributor branch.
- After pushing conflict repairs, refresh PR status, update the review worktree to the new head, and rerun the targeted validation that supports approval. Submit `APPROVE` only if the repaired head still has no blocking findings; otherwise submit `REQUEST_CHANGES` with the remaining conflict or validation problem.
- `BLOCKED` or `UNSTABLE` may remain because of CI or reviews even after conflicts are gone; do not treat that alone as failed conflict repair.
- If you performed only a conflict dry run or process exercise, do not push or submit a review. Record the PR number, approval-state nuance, conflicted files, semantic resolution, validation commands/results, and that no GitHub branch was changed; then abort/remove the conflict worktree unless the diagnostics must be kept.

## Review Focus

Review the PR against its stated intent, the current base branch, existing project patterns, and relevant external semantics. Understand the implementation logic, not just whether tests pass:

- POSIX/Linux behavior for syscalls, process/session/signal semantics, filesystem errors, sockets, IPv4/IPv6, and `/proc`.
- RFC or Linux behavior for networking details such as IPv6 NDP, IPv4-mapped IPv6, dual-stack listeners, route/listen conflicts, and errno behavior.
- VirtIO, PCI, DMA, MMIO, IRQ, and ownership rules for driver changes.
- Axvisor config semantics for `entry_point`, `kernel_load_addr`, `memory_regions`, `map_type`, and guest image layout.
- `starry-test-suit` rules when StarryOS test cases or `qemu-*.toml` files change.
- `cross-kernel-driver` architecture rules when portable driver crates or driver glue change.

For bug fixes, require a reproduction test that fails before the fix and passes after it unless the environment makes that impossible. For raw syscall fixes, prefer direct `syscall(SYS_...)` coverage when libc wrappers could mask return values or errno.

For PRs that add StarryOS app support, separate operator-facing app scenarios from CI-oriented semantic coverage:

- App-level smoke, demo, rootfs preparation, board/QEMU run scripts, and long-running or opt-in workflows belong under `apps/starry/<app-or-scenario>/`, following `apps/starry/README.md`.
- Kernel ABI, syscall, filesystem, process, networking, or other bugfix coverage exposed while enabling the app belongs under `test-suit/starryos/normal` in the matching existing case group, such as `qemu-smp1/syscall`, `qemu-smp1/bugfix`, `qemu-smp1/c-regression`, networking, DRM, evdev, or another closest semantic group.
- If the PR adds a syscall or changes syscall semantics for the app, require a minimal normal syscall/regression test that exercises the syscall surface directly; an app smoke passing is not enough.
- If the PR fixes a bug found through the app, require a normal bugfix/regression test that reproduces the bug without depending on the full app workflow whenever practical; keep the app scenario in `apps/starry` as integration evidence.
- Do not approve app-support PRs that put app workflows only into `test-suit/starryos/normal`, or that hide syscall/bugfix coverage only inside `apps/starry` demos.
- If the PR adds or changes an app-oriented Starry QEMU case under either `apps/starry` or `test-suit/starryos`, run the actual documented app command or exact `cargo xtask starry test qemu ... -c <case>` path in QEMU for at least the changed/claimed architecture. For multi-arch `qemu-*.toml` additions, run the architecture most likely to fail from CI or PR history; if any newly added required architecture is already failing in CI, reproduce or classify that architecture before approval.
- Do not approve when the app/test cannot be run as described by the PR, when its success depends on an unavailable or unstable external service without a controlled fallback, or when the command only passes on a narrower target than the PR claims. Report the exact command, architecture, guest-visible failure marker, and whether the failure matches remote CI.

Do not approve changes that are only shaped to satisfy the added tests, such as hard-coded special cases, skipped behavior, fake state updates, no-op compatibility shims, or logic that does not implement the intended subsystem semantics. Treat this as blocking even when local tests and CI pass.

Do not accept "success path" tests that silently skip on unexpected failure, such as returning early when `brk`, `sbrk`, I/O, or socket setup returns `ENOMEM`/`EAGAIN`, unless the test prints an explicit skip marker and the review explains why the environment legitimately cannot require success. Bugfix reproduction tests should fail loudly when the fixed behavior is absent.

Do not accept changes that simplify, skip, or weaken existing CI/test requirements unless the PR clearly justifies an equivalent or stronger replacement and the replacement is validated. Treat as blocking when a PR removes cases from normal groups, narrows architectures, loosens `success_regex`/`fail_regex`, converts failures into skips/timeouts, changes workflow path filters so relevant tests no longer run, or moves coverage from CI into an opt-in/manual path without preserving normal regression coverage.

### Crates.io Patch And Dependency Boundaries

When a PR touches `Cargo.toml`, `Cargo.lock`, dependency metadata, duplicate crate versions, third-party dependency APIs, or error-boundary code, inspect whether it adds, changes, or relies on a `[patch.crates-io]` override. Do not approve PRs that patch any crates.io dependency to a local path, fork, or git revision. This includes, but is not limited to, redirects like `[patch.crates-io] ax-errno = { path = "components/axerrno" }`.

Normal workspace dependency declarations, such as a workspace member using `{ path = "...", version = "..." }`, are not the same as a crates.io patch. The blocking case is overriding crates.io resolution for a dependency that another crate expects to get from the registry.

The preferred fix is to keep third-party dependencies using their normal crates.io resolution and adapt only at the local boundary:

- use the dependency crate's exported public types, traits, error types, or result aliases instead of referencing or replacing that dependency's internal dependency paths;
- add a crate-private adapter near the boundary when local code needs a local type, error, trait object, or ABI representation;
- replace implicit `?` conversions that cross dependency-local and workspace-local types with explicit `.map_err(...)`, `TryFrom`, wrapper newtypes, or a crate-private extension trait;
- keep dependency-facing trait/API code in the dependency's own exported types when it is still implementing or satisfying that dependency's public boundary;
- if the dependency itself is wrong, prefer an upstream fix or a normal dependency upgrade path, not a workspace-level crates.io patch in the PR.

For error-type mismatches, convert through stable public information exposed by the dependency. For example, when the dependency exports an errno-bearing error, convert the public code into the local errno/error type at the boundary and provide an explicit fallback for unknown values.

For the known `kbpf-basic`/Starry eBPF case, the review should suggest this shape instead of accepting an `ax-errno` patch: keep `kbpf-basic` on crates.io `ax-errno`; use `kbpf_basic::BpfError` and `kbpf_basic::BpfResult`; add a crate-private eBPF error adapter that converts `err.code()` into local `ax_errno::LinuxError` and then `ax_errno::AxError`; use that adapter in Starry eBPF/perf entry points that return local `AxResult`.

## Duplicate And Overlap Analysis

This analysis is required for every PR, not only bug fixes. Its purpose is to avoid approving duplicate implementations, stale rework, superseded fixes, or PRs that unknowingly conflict with another open PR.

Build an intent fingerprint before searching:

- PR title, body, linked issue numbers, commit subjects, and author-stated validation.
- Changed crates, modules, test cases, configs, CI files, and generated assets.
- Public APIs, syscall names, errno behavior, protocol terms, device types, runner commands, test binary names, and feature flags touched by the patch.
- The semantic claim being made: new feature, bug fix, test coverage, refactor, config update, CI repair, or dependency/metadata change.

Check current base branch first. Search for equivalent behavior, tests, config entries, public APIs, or previous fixes already present on `origin/<base>`:

```bash
git grep -n -E '<relevant symbols|paths|commands>' origin/<base> -- <likely paths>
git log --oneline --decorate -- <likely paths>
```

If base already has the same behavior or a newer version of it, treat the PR as stale or duplicate unless it clearly adds distinct value. Verify that distinction by reading the relevant base code, not just matching names.

Then check related open PRs. Use the GitHub MCP/connector to search or list candidate PRs before falling back to `gh`. Search with multiple terms derived from the intent fingerprint; do not rely only on the PR title. Useful terms include crate/module names, changed path fragments, syscall or API names, test case names, issue numbers, errno values, protocol/device names, CI job names, and config names.

```bash
gh pr list --state open --limit 200 --search '<symbol OR path OR issue keyword>'
gh pr view <related-pr> --json number,title,body,author,baseRefName,headRefName,isDraft,updatedAt,files,commits
gh pr diff <related-pr> --patch --color=never
git diff --name-only origin/<base>...origin/pr/<related-pr>
```

Inspect each plausible related PR enough to classify it:

- `duplicate`: solves the same problem or adds the same test/API/config behavior with no meaningful distinction.
- `partial-overlap`: touches the same surface but the changes are complementary, ordered, or separable.
- `conflict-risk`: likely merge or semantic conflict because both PRs modify the same contract, runner behavior, generated asset, or ABI expectation.
- `superseded`: another PR or current base implements the same intent more completely or in a better-aligned way.
- `unrelated-after-inspection`: matched search terms but does not overlap after reading files/diff/intent.

For `partial-overlap` or `conflict-risk`, compare the implementation direction with project semantics and note the expected merge order or follow-up needed. If correctness depends on another PR landing first, do not approve until that dependency is explicit in the PR body or review outcome. For `duplicate` or `superseded`, submit `REQUEST_CHANGES` or leave a neutral project-focused comment explaining which base code or open PR should be preferred and why.

Use `git diff origin/<base>...origin/pr/<pr>` for the PR patch. Use `origin/<base>..origin/pr/<pr>` only when intentionally checking stale-branch effects.

Treat a PR as not mergeable when it is superseded by a more complete PR or would regress newer base-branch work. Leave a neutral project-focused comment explaining why the newer PR or base implementation should be preferred. If asked to close such a PR, prefer `gh pr comment <pr> --body-file comment.md` followed by `gh pr close <pr>`; avoid shell backticks in inline `--comment` strings.

## Validation

Run focused validation matching the changed surface. Prefer project `xtask` commands:

```bash
cargo fmt --check
cargo xtask clippy --package <crate>
cargo clippy --manifest-path <path>/Cargo.toml --all-features -- -D warnings
cargo xtask starry test qemu --arch <arch> -c <case>
cargo xtask axvisor build ... --vmconfigs <config>
```

If `cargo xtask` does not cover a special configuration, inspect the relevant `xtask` help or source before falling back to native Cargo with matched arguments. Record exact commands and failures.

For dependency metadata changes, inspect dependency resolution instead of relying only on the diff. Check for any crates.io patch first, then inspect the affected dependency subtree:

```bash
rg -n '\[patch\.crates-io\]' -g 'Cargo.toml' .
cargo metadata --format-version=1 | jq -r '.packages[] | [.name,.version,.source,.manifest_path] | @tsv' | rg '<affected-crate>'
cargo tree -p <affected-package> | rg '<affected-crate>|<boundary-crate>'
```

For the `kbpf-basic`/`ax-errno` example, useful focused checks are:

```bash
cargo metadata --format-version=1 | jq -r '.packages[] | select(.name=="ax-errno") | [.version,.source,.manifest_path] | @tsv'
cargo tree -p starry-kernel | sed -n '/kbpf-basic v0.5.7/,+12p'
```

The expected result for that example is that local workspace crates still use local `components/axerrno`, while `kbpf-basic` resolves its own crates.io `ax-errno` and local Starry eBPF/perf code performs explicit error conversion at the boundary.

For StarryOS grouped QEMU cases, verify that new `test_commands` are actually discovered and installed into the guest overlay. A `qemu-*.toml` command such as `/usr/bin/<test>` must correspond to a case/subcase asset path that the runner discovers and builds. Running the containing grouped case is the preferred check, for example `cargo xtask starry test qemu --arch x86_64 -c syscall`. Treat `/usr/bin/<test>: not found`, `status=127`, skipped discovery, unbuilt asset directories, unreliable `success_regex`/`fail_regex`, or tests that accept both broken and fixed behavior as blocking.

For bugfix tests in grouped cases, inspect the new test's assertions as well as running the case. A grouped case passing is not sufficient when the new test accepts both the fixed behavior and the broken behavior.

For StarryOS app-support PRs, validate both sides when both are present:

- Run the relevant `apps/starry` command or an equivalent documented app workflow when the PR adds or changes app support, unless it needs unavailable hardware, credentials, or long-running services; record any limitation.
- Run the corresponding `cargo xtask starry test qemu --test-group normal ...` case when the PR adds a syscall, fixes a kernel/runtime bug, or claims normal test coverage. App validation does not replace normal regression validation.
- If the app scenario and normal regression cover different risks, mention both results in the review body.
- Do not stop at `--list`, TOML parsing, script inspection, or another reviewer saying an older head passed. Those checks prove discovery only, not that the app works. Run the current head in QEMU whenever the changed app/test is intended to run in QEMU.
- If `tmp/axbuild/rootfs` is empty, still try the relevant `cargo xtask starry rootfs --arch <arch>` or `cargo xtask starry test qemu ...` path before declaring QEMU unavailable; the xtask flow can download managed rootfs images automatically. Record a blocker only after the xtask download/run path itself fails for an environmental reason.
- Do not run multiple Starry QEMU cases concurrently in one worktree. Run one architecture/case to completion, then move to the next architecture if needed.

When the PR does not add or modify a test case, inspect the PR body and commit messages for any claimed non-board validation method, such as QEMU, host unit tests, `cargo xtask`, `cargo test`, `cargo clippy`, shell scripts, emulators, or reproducible manual commands that do not require physical hardware:

- If such validation is claimed, run it or an equivalent local command before approval. Compare the actual command, target, output, and pass/fail condition with the PR's claim.
- If the claimed validation fails, is not reproducible as written, exercises a different target than claimed, silently skips the changed behavior, or cannot be run for an avoidable reason, submit `REQUEST_CHANGES`. Explain the mismatch and the expected fix direction: either make the validation true and reproducible, add an appropriate test, or correct the PR description.
- If the claimed validation cannot be run because the environment is genuinely unavailable, record the exact limitation and do not treat the claim as proof. Require another reproducible non-board validation method or a test unless the user explicitly accepts the limitation.
- If the PR has no test changes and neither the PR body nor commit messages describe a reproducible non-board validation method, do not approve. Request changes asking the author to add a test or document and provide a runnable validation command that covers the changed behavior.
- Physical board-only validation may be useful evidence, but it does not satisfy this no-test fallback rule by itself unless the user explicitly scopes the review to board-only behavior.

Use GitHub check status as required evidence, but not as the only review input:

```bash
gh pr checks <pr> --watch=false
```

Do not approve solely because remote CI passes. Conversely, if required checks are failing, cancelled, or missing for a branch that needs CI coverage, inspect logs and classify the failure before deciding. Treat PR-related CI failures as blocking and request changes with the expected fix direction. If a CI failure is unrelated to the PR, it is not by itself a reason to request changes, but the review body must say why it is unrelated and link an existing or newly-created tracking issue. A branch with no reported checks is not equivalent to passing; require targeted local validation before approving, and request changes when the changed surface is too large or risky to validate locally.

When GitHub log download fails or returns an empty log, do not infer the check passed or was irrelevant. Use `gh pr checks <pr> --repo <owner>/<repo> --watch=false` and `gh run view <run-id> --json headSha,jobs` to confirm the current head, failing job names, conclusions, and failing steps. If the failing job matches a newly added or changed app/test architecture, treat it as PR-related unless concrete evidence proves otherwise.

## Blocking Findings

Treat these as blocking unless clearly non-blocking:

- behavior differs from POSIX/Linux/RFC/VirtIO semantics;
- targeted tests, formatting, clippy, or PR-related CI fail;
- a newly added or changed Starry app/QEMU case fails when run as described by the PR, including one architecture among newly added multi-arch `qemu-*.toml` cases;
- a PR claims app/QEMU support but only discovery, TOML parsing, or an older-head run was validated;
- new tests are not discovered by the project test runner or do not exercise the fixed ABI surface;
- a PR has no test changes and lacks a reproducible non-board validation method in the PR body or commit messages;
- a claimed non-board validation method is not actually reproducible or does not match the claimed coverage/result;
- `success_regex` or `fail_regex` cannot reliably classify the intended StarryOS case result;
- bug fixes lack meaningful reproduction coverage;
- the PR adds, changes, or relies on `[patch.crates-io]` to redirect any crates.io dependency to a local path, fork, or git revision, instead of adapting through the dependency's exported public API or an explicit local boundary adapter;
- merge conflicts are unresolved, conflict repair resurrects outdated base APIs instead of adapting PR intent to current base, or the repaired head was not revalidated after push;
- StarryOS app-support PRs place app workflows under `test-suit/starryos/normal` instead of `apps/starry`, or place syscall/bugfix semantic coverage only under `apps/starry` instead of the matching normal test-suit case;
- the implementation is a test-only or fake fix that does not implement the intended behavior;
- submitted buffers, DMA memory, queue tokens, or IRQ ownership can leak, be freed too early, or cross the wrong abstraction layer;
- a change silently makes CI hang, time out, or skip the new coverage;
- a change weakens CI or normal-regression coverage by removing cases, narrowing architectures, loosening pass/fail regexes, skipping relevant workflows, or moving required coverage to manual-only paths without an equivalent validated replacement;
- the PR duplicates existing base-branch behavior, weakens an existing implementation, conflicts with a related open PR, or is superseded by a newer base-branch or open-PR fix;
- the review cannot explain how this PR differs from a plausible related open PR after duplicate and overlap analysis.

All GitHub review text, including inline comments, review body, and replies, must be in Chinese, neutral, and project-focused. Each blocking comment should include the concrete problem, the relevant standard/project rule/observed failure, and a suggested fix.

Prefer changed lines on the PR diff. Before submitting, verify every inline `line` exists on the current right side of the diff; if GitHub cannot resolve a line, move to the nearest changed line that demonstrates the issue or put the finding in the review body. Context or unchanged lines may be rejected by the review API.

## Submit Review

Before submitting, confirm through the GitHub MCP/connector that the PR head SHA has not changed. Fallback only when connector data is unavailable:

```bash
gh pr view <pr> --json number,headRefOid,reviewDecision
```

If the head changed after analysis or validation, fetch the new head, update the worktree, re-check each finding on current changed lines, and rerun the targeted validation that supports the decision.

Submit the final review through the GitHub MCP/connector when it can send the review event and inline comments with preserved anchors together. If the connector cannot submit inline review comments or cannot preserve line anchors, fallback to the GitHub review REST API via `gh`:

```bash
gh api --method POST repos/<owner>/<repo>/pulls/<pr>/reviews --input review.json
```

Use the current `headRefOid` as `commit_id`, `side=RIGHT` for inline comments, `REQUEST_CHANGES` for any blocking issue, and `APPROVE` only when no blocking issue remains:

```json
{
  "commit_id": "<headRefOid>",
  "event": "REQUEST_CHANGES",
  "body": "...",
  "comments": [
    {"path": "path/to/file.rs", "line": 123, "side": "RIGHT", "body": "..."}
  ]
}
```

Do not submit stale findings against an old head.

If a worker returns a finding on a line that is not present on the current PR diff, move the comment to the nearest changed line that demonstrates the problem or put the finding in the review body.

After submission, re-query the PR. If a new commit landed during review submission, refresh the worktree and submit a follow-up review only if the blocking issue still applies to the new head.

Review body must explain in Chinese:

- what the PR changed;
- the implementation logic and why this approach is correct for the project semantics;
- validation commands and results, including exact failure mode for failing tests;
- when no tests are added, the PR body/commit-message validation claim that was checked, the command actually run, and whether it matched the claim;
- CI status, including any unrelated failing checks, the evidence for unrelatedness, and the linked tracking issue;
- duplicate and overlap analysis: base-branch evidence checked, related open PRs inspected, and why the PR is distinct, complementary, duplicate, conflicting, or superseded;
- conflict handling status when applicable: conflicted files, resolution logic, validation after repair, and whether a repair commit was pushed or the work was intentionally kept as a dry run;
- for PR-related CI failures, the failing check, failure mode, and expected fix direction;
- reproduction coverage status for bug fixes;
- unresolved review conversations that were resolved, and conversations intentionally left open and why;
- any behavior that remains unimplemented, partial, or should be completed in future work;
- any known environment limitation.

Do not approve when the review cannot explain the implementation logic beyond "tests pass".

Verify final state:

```bash
gh pr view <pr> --json number,reviewDecision,latestReviews
```

## Post-Review Reviewer Assignment

After review submission, decide whether the PR still needs human reviewer requests. Do this after the technical review so reviewer choice is based on the actual changed surface, duplicate/overlap findings, validation risk, and remaining follow-up.

Use discussion 594 as the reviewer source of truth. Read the current table directly before assigning because personnel directions may change:

```bash
gh api graphql \
  -f query='query($owner:String!,$repo:String!,$number:Int!){ repository(owner:$owner,name:$repo){ discussion(number:$number){ title body url comments(first:100){nodes{author{login} body createdAt}} } } }' \
  -F owner=rcore-os -F repo=tgoskits -F number=594
```

Map PR content to reviewer directions from the "人员方向整理" table:

- StarryOS tests, `test-suit/starryos`, QEMU cases, rootfs/app tests, `apk`, distro behavior, or `axbuild` test flow: prefer reviewers covering `测试`, `发行版/rootfs`, `axbuild`, and the relevant `starry` area.
- Syscall, filesystem, network, driver, platform, architecture, CI, documentation, and display changes should be mapped to the matching table columns, then cross-checked against changed files and PR body claims.
- If a PR matches several domains, request one primary reviewer for the highest-risk domain and one secondary reviewer for integration or test coverage. Avoid over-requesting reviewers.
- Drop the PR author from targets. Preserve existing bot review requests and unrelated existing human reviewer requests unless the user explicitly asks to rebalance them.

For StarryOS normal QEMU app tests like PR 795 (`test-suit/starryos/normal/qemu-smp1/git`, `apk add`, app/rootfs behavior), a good mapping is `测试` + `发行版/rootfs` + `starry`: `@ZCShou` for test/rootfs/axbuild ownership and `@luodeb` for Starry/rootfs experience. Use this as a pattern, not as a hard-coded rule; still inspect the current discussion table and PR contents.

Before writing reviewer requests, check current requested reviewers and permissions:

```bash
gh api repos/rcore-os/tgoskits/pulls/<pr>/requested_reviewers
gh api repos/rcore-os/tgoskits/collaborators/<login>/permission
```

Use the REST requested-reviewers API instead of `gh pr edit`, because `gh pr edit` can fail in this repository while querying deprecated Projects classic fields:

```bash
printf '%s\n' '{"reviewers":["<login1>","<login2>"]}' |
  gh api -X POST repos/rcore-os/tgoskits/pulls/<pr>/requested_reviewers --input -
```

After assigning, re-query `requested_reviewers` and confirm the intended reviewers are present. If GitHub rejects a reviewer, record the exact login and API or permission error; do not silently substitute someone not supported by discussion 594.

In the final user summary, state:

- which reviewer direction columns matched the PR;
- which reviewers were requested, already present, skipped, or rejected;
- any permission/API limitation;
- that only GitHub reviewer metadata was changed, when no code files were edited by the assignment step.

## Cleanup

After review submission or an explicit no-submit stop, clean temporary resources before ending:

- Remove clean review and conflict worktrees with `git worktree remove <path>`, then run `git worktree prune` from the main repository.
- Delete temporary files created for review payloads, GraphQL queries, comments, logs, or conflict notes unless the user asked to keep them.
- Do not remove a worktree that has uncommitted conflict-repair work, diagnostics needed for a reported failure, or user-created changes; report the path and reason instead.
- Confirm the main worktree status was not changed by the review workflow.
