# AGENTS.md

## Project Skills

- `update-std-tests`: project-local skill at `.claude/skills/update-std-tests/SKILL.md`
- Use `update-std-tests` when the user wants to audit or update `scripts/test/std_crates.csv`, compare workspace packages against the std test whitelist, or confirm which new std-test candidates should be added.
- `starry-test-suit`: project-local skill at `.claude/skills/starry-test-suit/SKILL.md`
- Use `starry-test-suit` when the user wants to add, regroup, adapt, or validate `test-suit/starryos` cases, including `qemu-*.toml`, `normal`/`stress` grouping, success/fail regexes, or Starry test-suit related CI behavior.
- `cross-kernel-driver`: project-local skill at `.claude/skills/cross-kernel-driver/SKILL.md`
- Use `cross-kernel-driver` when the user wants to create, refactor, review, or optimize portable Rust driver crates under `drivers/` by device type, separate Driver Core / Capability Boundary / OS Glue / Runtime layers, handle MMIO/iomap with `mmio-api`, handle DMA with `dma-api`, design IRQ event or queue contracts, or audit OS API coupling in driver code.
- `review-open-prs`: project-local skill at `.claude/skills/review-open-prs/SKILL.md`
- Use `review-open-prs` when the user wants to audit all open GitHub PRs, review non-self PRs, re-review PRs updated after their last review, use subagents/worktrees for PR review, compare changes with POSIX/Linux/RFC/VirtIO semantics, run local validation, and submit approve or request-changes reviews.
- `review-single-pr`: project-local skill at `.claude/skills/review-single-pr/SKILL.md`
- Use `review-single-pr` when the user names one PR number or URL and wants a focused review, re-review, duplicate or overlapping open-PR analysis, Starry app-support test placement checks, merge-conflict handling for otherwise approvable PRs, Linux/POSIX/RFC/VirtIO comparison, local validation, Chinese inline review comments, approval, request-changes submission, or post-review reviewer assignment.
- `reassign-pr-reviewers`: project-local skill at `.claude/skills/reassign-pr-reviewers/SKILL.md`
- Use `reassign-pr-reviewers` when the user wants to assign or rebalance GitHub PR reviewers for `rcore-os/tgoskits` from a discussion, ownership matrix, open PR scope, or existing review-request state, including preserving bot requests and handling collaborator permission limits.
- `board-uboot-fsck-repair`: project-local skill at `.claude/skills/board-uboot-fsck-repair/SKILL.md`
- Use `board-uboot-fsck-repair` when a physical board Linux rootfs needs ext4 recovery through U-Boot, initramfs fsck reports unrepaired corruption, OrangePi-5-Plus needs `extraboardargs=fsckfix`, or Starry board write tests must be bracketed by Linux fsck/boot checks.
- `board-linux-starry-debug`: project-local skill at `.claude/skills/board-linux-starry-debug/SKILL.md`
- Use `board-linux-starry-debug` when a physical-board workflow needs Linux-side deployment or inspection before running StarryOS or ArceOS, including `board connect` IP discovery, SSH/rsync while holding a board lease, explicit `sync` before rebooting into StarryOS, diagnosing StarryOS `not found` for files copied into the Linux rootfs, or comparing Linux-visible and StarryOS-visible board rootfs state.
- `crates-io-owner`: project-local skill at `.claude/skills/crates-io-owner/SKILL.md`
- Use `crates-io-owner` when the user wants to add or verify `github:rcore-os:crates-io` for branch-added crates, asks which new crates still need the crates.io team owner, or explicitly wants `cargo owner` used instead of `Cargo.toml` metadata.
- `arch-platform-porting`: project-local skill at `.claude/skills/arch-platform-porting/SKILL.md`
- Use `arch-platform-porting` when the user wants to add, adapt, debug, or review architecture/platform support for ArceOS, StarryOS, Axvisor, someboot, dynamic UEFI platform boot, SMP startup, QEMU boot configs, target JSON files, axbuild arch mapping, axcpu trap/context code, axplat-dyn, somehal, or LoongArch/x86/aarch64/riscv platform bring-up issues.

## Other Requirements

- When changing logic, run a relevant `cargo clippy` check after the code change.
- After modifying a crate, ensure that crate passes clippy. Prefer `cargo xtask clippy --package <crate>` for targeted verification.
- Do not silence clippy warnings with `allow` as a shortcut; prefer fixing the root cause unless the user explicitly asks otherwise.
- Run `cargo fmt` after code edits.
- For ArceOS, StarryOS, and Axvisor builds/tests/runs, prefer the `cargo xtask` command family instead of raw `cargo build`, `cargo test`, or `cargo run`.
- If `cargo xtask` cannot satisfy a special configuration, inspect the `xtask` flow first and only then fall back to native Cargo commands with manually matched arguments.
- When resolving rebase or merge conflicts, do not manually merge conflicted `Cargo.lock` contents. Resolve all other conflicts first, then regenerate `Cargo.lock` with Cargo and verify the generated lockfile.
- When reviewing a PR, fully read (完整阅读) `.claude/skills/review-single-pr/SKILL.md` before judging merge readiness, drafting comments, approving, requesting changes, or posting a no-submit summary.
- During PR review, build a todo/checklist from the full `review-single-pr` requirements and verify each applicable merge requirement one by one. Mark each item as satisfied, not applicable with a concrete reason, or blocking with evidence.
- For PRs, issues, review replies, discussions, and similar project-facing submissions, keep the language neutral and project-focused.
- For PR titles, follow `type(scope): content` in Conventional Commits style. Prefer the main affected crate name as `scope` when one crate clearly dominates the change; for cross-cutting or infrastructure work, broader scopes such as `ci`, `repo`, or `docs` are acceptable.
- PR title examples: `feat(axbuild): add Starry remote board test flow`, `fix(starry-process): correct tty session cleanup`, `chore(ci): split Starry self-hosted board matrix`.
- When submitting a PR, write the title in English and the body in Chinese.
- PR descriptions must clearly cover: the problem being solved, what was changed to solve it, and the logic behind each step of the solution.
- Before submitting a PR, locally validate the CI flow as much as practical, excluding only physical board tests and self-hosted test flows unless the user explicitly asks to run them. Changes unrelated to building or testing, such as documentation-only updates, do not require local CI validation.
- After adding or changing commits on a PR branch, update the PR description so it stays synchronized with the committed changes.
- Do not insert agent-related labels, signatures, branding, or other advertisement-style wording such as `codex`, `agent`, `AI`, or similar self-promotional tags unless the user explicitly requests it.
- When changing architecture boot logic, someboot startup order, UEFI handoff, SMP bring-up, dynamic platform contracts, target JSON assumptions, or the recommended debugging flow, update `.claude/skills/arch-platform-porting/SKILL.md` or its references in the same change.
