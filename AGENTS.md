# AGENTS.md

<!-- SPECKIT START -->
**Active Plan**: `specs/003-starryos-nixpkgs/plan.md` — Complete Linux-Semantics Fixes for Nix Sandbox Blockers (mount.rs, namespace.rs, proc.rs)
<!-- SPECKIT END -->

## Project Skills

- `update-std-tests`: project-local skill at `.claude/skills/update-std-tests/SKILL.md`
- Use `update-std-tests` when the user wants to audit or update `scripts/test/std_crates.csv`, compare workspace packages against the std test whitelist, or confirm which new std-test candidates should be added.
- `starry-test-suit`: project-local skill at `.claude/skills/starry-test-suit/SKILL.md`
- Use `starry-test-suit` when the user wants to add, regroup, adapt, or validate `test-suit/starryos` cases, including `qemu-*.toml`, `normal`/`stress` grouping, success/fail regexes, or Starry test-suit related CI behavior.
- `cross-kernel-driver`: project-local skill at `.claude/skills/cross-kernel-driver/SKILL.md`
- Use `cross-kernel-driver` when the user wants to create, refactor, review, or optimize portable Rust driver crates under `drivers/` by device type, separate Driver Core / Capability Boundary / OS Glue / Runtime layers, handle MMIO/iomap with `mmio-api`, handle DMA with `dma-api`, design IRQ event or queue contracts, or audit OS API coupling in driver code.
- `review-open-prs`: project-local skill at `.claude/skills/review-open-prs/SKILL.md`
- Use `review-open-prs` when the user wants to audit all open GitHub PRs, review non-self PRs, re-review PRs updated after their last review, use subagents/worktrees for PR review, compare changes with POSIX/Linux/RFC/VirtIO semantics, run local validation, and submit approve or request-changes reviews.
- `resolve-github-issue`: project-local skill at `.claude/skills/resolve-github-issue/SKILL.md`
- Use `resolve-github-issue` when the user wants to inspect the latest or a specified GitHub issue, analyze and fix the root cause instead of loosening tests, use subagents for issue investigation or patch review, add deterministic regression coverage, validate the original failing command, submit a PR, or include `Fixes #<issue>` so merging closes the issue.
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

## Rust Coding Standards

- Before writing, modifying, or reviewing code, fully read (完整阅读) every file under `book/guideline/` and treat those documents as mandatory coding standards. If the conversation context is compacted, resumed from a summary, or you cannot confidently recall the guideline contents, re-read all files under `book/guideline/` before continuing so the coding rules are not forgotten.
- Use the pinned Rust 2024 nightly toolchain and the repository rustfmt configuration as the formatting source of truth; do not restate rustfmt-owned layout rules in prose.
- Prefer `#![no_std]` for reusable kernel, component, memory, virtualization, and portable driver crates; add `alloc`, `std`, or feature-gated support only where the crate boundary requires it.
- Keep crate and module boundaries aligned with TGOSKits layers: reusable logic belongs in `components/`, `drivers/`, `memory/`, or `virtualization/`; OS glue belongs near the consuming ArceOS, StarryOS, Axvisor, or platform layer.
- Write code so it can pass the applicable `.claude/skills/review-single-pr/SKILL.md` review lenses: maintainability, correctness, security/soundness, hardware/ABI, and documentation/user-facing compatibility. Treat those lenses as author-side design constraints, not only reviewer-side checks after the fact.
- Keep modules domain-focused. Use private implementation modules by default, expose only intentional public surfaces, and re-export stable entry points from `lib.rs` when that improves the public API.
- Name items by their domain invariant, such as address space, IRQ line, VM, device, queue, request, page, frame, capability, or error condition. Avoid generic names like `data`, `info`, `mgr`, or `handle` when a stronger project concept is known.
- Prefer small functions that perform one state transition, hardware operation, syscall step, validation step, or conversion. Split probe/map/register/enable flows into named phases when each phase has distinct invariants or failure handling.
- Make mutation and side effects visible through `&mut`, returned values, typed state transitions, or clearly named APIs. Avoid boolean-heavy control flags when separate functions, enums, or configuration structs express the intent better.
- Prefer typed IDs, newtypes, `repr(transparent)` wrappers, const constructors, operation enums, and bitflags over raw `usize`, strings, or loosely related parameters.
- Separate plain data from behavior-owning objects. Configuration, descriptors, and wire-format data may expose fields; types that own invariants, resources, locks, or hardware state should keep representation private and expose intent-revealing methods.
- Split large objects by reason to change and by owned invariant. Prefer separate types for immutable configuration, validated descriptors, mutable runtime state, queues, IRQ endpoints, capability handles, and OS adapters when those parts have different lifetimes or synchronization rules.
- Use traits as small capability boundaries, not inheritance hierarchies. Expose the capability the consumer needs, and prefer extension traits, adapter types, or feature-gated APIs over growing a central trait for optional behavior.
- Prefer composition over inheritance-shaped designs. Build larger services from named parts such as control ports, queues, backends, allocators, registries, and adapters; use concrete fields or generics for static composition and trait objects only at dynamic capability or plugin boundaries.
- Do not force callers to reach through nested objects to perform work. Keep internal parts private when they are implementation details, and expose small methods that express the boundary action, state transition, or query the caller actually needs.
- Keep driver cores independent from OS runtime glue. MMIO, DMA, IRQ, queue, wake, poll, and task-scheduling contracts should cross explicit capability boundaries such as `mmio-api`, `dma-api`, `rdif-*`, or runtime adapter layers.
- Use workspace package names and `[workspace.dependencies]` where available. Prefer workspace metadata, disable default features for `no_std` dependencies unless required, and avoid ad hoc git/path/registry overrides.
- Library and domain crates should expose typed errors that callers can match and translate. Nontrivial public error enums in library, component, domain, and hardware-abstraction crates should derive `thiserror::Error` from the workspace `thiserror` dependency and put display text in `#[error(...)]`; only tiny, strongly dependency-sensitive crates should hand-write `Display` and `core::error::Error`.
- Host-side `bin` and tool crates should use `anyhow::Result`, `Context`, `anyhow!`, and `bail!` for top-level orchestration and human-facing error reports. Do not leak `anyhow::Error` into reusable library APIs; translate typed domain errors to `ax_errno::{AxError, AxResult}` at ArceOS or kernel integration boundaries.
- Return explicit unsupported or error variants for unimplemented platform, firmware, hardware, guest, user-memory, filesystem, and network paths. Do not silently fall back, guess a default device/IRQ/address, or stringify structured metadata when callers need to make a decision.
- Use `unwrap`, `expect`, and `panic` only in tests, impossible-state assertions, one-time initialization failures, or documented invariants. Recoverable runtime failures should return `Result` or `Option` with enough context for translation or retry.
- Keep `unsafe` blocks as small as practical and place checked preconditions next to them. Every `unsafe fn` or `unsafe trait` needs a `# Safety` contract; every nontrivial `unsafe` block or `unsafe impl` should document pointer validity, aliasing, MMIO/DMA ownership, user-memory access, interrupt context, or lifetime assumptions.
- For concurrency, choose repo primitives deliberately: sleepable locks for sleepable paths, IRQ-aware or non-sleeping locks for interrupt and scheduler-sensitive paths, and narrow critical sections. Document lock ordering when a module owns multiple locks, and avoid wake/notify callbacks while holding broad locks.
- Use atomics with explicit publish/observe reasoning. Prefer Acquire/Release/AcqRel for synchronization; use `Relaxed` only for counters or proven non-synchronizing state, with the synchronization path documented where it is not obvious.
- Comments should explain invariants, safety contracts, protocol steps, hardware quirks, concurrency ordering, or non-obvious tradeoffs; do not restate the code. Public APIs and shared code comments should be in English.
- Remove duplicated knowledge, not every repeated line. Centralize protocol constants, layout rules, error conversions, and boundary invariants, but avoid premature abstractions that hide control flow or make call sites harder to audit.
- Refactor in small verified steps. Keep behavior stable unless the change intentionally updates semantics, and pair risky refactors with the lowest-layer deterministic regression or validation that can catch a breakage.

## Other Requirements

- When changing logic, run a relevant `cargo clippy` check after the code change.
- After modifying a crate, ensure that crate passes clippy. Prefer `cargo xtask clippy --package <crate>` for targeted verification.
- Do not silence clippy warnings with `allow` as a shortcut; prefer fixing the root cause unless the user explicitly asks otherwise.
- Run `cargo fmt` after code edits.
- When fixing a bug, first add a deterministic regression test that necessarily fails on the buggy implementation, verify the failure, then implement or restore the fix and verify the same test passes. Do not rely only on post-fix validation, probabilistic reproducers, or relaxed tests.
- For self-hosted CI matrix entries in `.github/workflows/ci.yml`, keep `cache_key` as an empty string (`cache_key: ""`). Non-empty values enable the rust-cache step on self-hosted runners, which can remove Rust/Cargo state and break later jobs.
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
