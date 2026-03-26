# Project Skills

- When changing logic, run a relevant `cargo clippy` check after the code change.
- Do not silence clippy warnings with `allow` as a shortcut; prefer fixing the root cause unless the user explicitly asks otherwise.
- Run `cargo fmt` after code edits.

- `update-std-tests`: project-local skill at `.claude/skills/update-std-tests/SKILL.md`
- Use `update-std-tests` when the user wants to audit or update `scripts/test/std_crates.csv`, compare workspace packages against the std test whitelist, or confirm which new std-test candidates should be added.
