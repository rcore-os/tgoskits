# Project Skills

- When changing logic, run a relevant `cargo clippy` check after the code change.
- Do not silence clippy warnings with `allow` as a shortcut; prefer fixing the root cause unless the user explicitly asks otherwise.
- Run `cargo fmt` after code edits.
- For ArceOS, StarryOS, and Axvisor builds/tests/runs, prefer the `cargo xtask` command family instead of raw `cargo build`, `cargo test`, or `cargo run`.
- If `cargo xtask` cannot satisfy a special configuration, inspect the `xtask` flow first and only then fall back to native Cargo commands with manually matched arguments.
- For PRs, issues, review replies, discussions, and similar project-facing submissions, keep the language neutral and project-focused.
- Do not insert agent-related labels, signatures, branding, or other advertisement-style wording such as `codex`, `agent`, `AI`, or similar self-promotional tags unless the user explicitly requests it.

- `update-std-tests`: project-local skill at `.claude/skills/update-std-tests/SKILL.md`
- Use `update-std-tests` when the user wants to audit or update `scripts/test/std_crates.csv`, compare workspace packages against the std test whitelist, or confirm which new std-test candidates should be added.
