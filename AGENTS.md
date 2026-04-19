# AGENTS.md

## Project Skills

- `update-std-tests`: project-local skill at `.claude/skills/update-std-tests/SKILL.md`
- Use `update-std-tests` when the user wants to audit or update `scripts/test/std_crates.csv`, compare workspace packages against the std test whitelist, or confirm which new std-test candidates should be added.
- `starry-test-suit`: project-local skill at `.claude/skills/starry-test-suit/SKILL.md`
- Use `starry-test-suit` when the user wants to add, regroup, adapt, or validate `test-suit/starryos` cases, including `qemu-*.toml`, `normal`/`stress` grouping, success/fail regexes, or Starry test-suit related CI behavior.

## Other Requirements

- When changing logic, run a relevant `cargo clippy` check after the code change.
- After modifying a crate, ensure that crate passes clippy. Prefer `cargo xtask clippy --package <crate>` for targeted verification, and if the crate now passes but is missing from `scripts/test/clippy_crates.csv`, add it in the same change.
- Do not silence clippy warnings with `allow` as a shortcut; prefer fixing the root cause unless the user explicitly asks otherwise.
- Run `cargo fmt` after code edits.
- For ArceOS, StarryOS, and Axvisor builds/tests/runs, prefer the `cargo xtask` command family instead of raw `cargo build`, `cargo test`, or `cargo run`.
- If `cargo xtask` cannot satisfy a special configuration, inspect the `xtask` flow first and only then fall back to native Cargo commands with manually matched arguments.
- For PRs, issues, review replies, discussions, and similar project-facing submissions, keep the language neutral and project-focused.
- For PR titles, follow `type(scope): content` in Conventional Commits style. Prefer the main affected crate name as `scope` when one crate clearly dominates the change; for cross-cutting or infrastructure work, broader scopes such as `ci`, `repo`, or `docs` are acceptable.
- PR title examples: `feat(axbuild): add Starry remote board test flow`, `fix(starry-process): correct tty session cleanup`, `chore(ci): split Starry self-hosted board matrix`.
- Do not insert agent-related labels, signatures, branding, or other advertisement-style wording such as `codex`, `agent`, `AI`, or similar self-promotional tags unless the user explicitly requests it.
