---
name: update-std-tests
description: Audit and update `scripts/test/std_crates.csv` in this repository. Use when Codex needs to compare the current workspace's host `cargo test -p PACKAGE` results against the std test whitelist, summarize missing std-test candidates, ask whether passing or failing candidates should be added, or rewrite the CSV after user confirmation.
---

# Update Std Tests

Use this skill when the user wants to audit or refresh the std test whitelist for this repo.

## Quick Start

- Run `python3 scripts/std_test_candidates.py audit --repo-root <repo-root> --format markdown`.
- Show `Passing candidates` first and ask whether to add all of them.
- Then show `Failing candidates` and let the user choose `all`, `ignore`, or a comma-separated subset.
- Only run `python3 scripts/std_test_candidates.py apply --repo-root <repo-root> --packages ...` after the user confirms the exact additions.
- If only passing candidates are added, suggest validating with `cargo xtask test std`.

## Workflow

1. Run the audit script from the skill directory or with an absolute path.
2. Present the `Passing candidates` section before anything else.
3. If the environment exposes `request_user_input`, prefer it for confirmations. Otherwise ask a short plain-text question.
4. Present `Failing candidates` as a second decision point. Treat these as opt-in additions because they will currently fail `cargo xtask test std`.
5. Apply only the user-approved packages.
6. Re-run the audit or inspect `scripts/test/std_crates.csv` after applying changes.

## Candidate Policy

- Candidate source: workspace packages from `cargo metadata --no-deps`.
- Existing whitelist: `scripts/test/std_crates.csv` with a single `package` column.
- Include for auditing: `lib` packages and examples/bin-only packages.
- Exclude by default: `tg-xtask`, `axlibc`, `arm_vcpu`, `riscv_vcpu`, `axvisor`.
- Split remaining packages by full `cargo test -p <package>` result, not `--no-run`.

Read `references/filtering.md` before changing the filtering policy or explaining why a package lands in `passing`, `failing`, or `excluded`.

## Commands

Audit in Markdown:

```bash
python3 scripts/std_test_candidates.py audit --repo-root /path/to/repo --format markdown
```

Audit in JSON:

```bash
python3 scripts/std_test_candidates.py audit --repo-root /path/to/repo --format json
```

Apply confirmed packages:

```bash
python3 scripts/std_test_candidates.py apply --repo-root /path/to/repo --packages arceos-helloworld arceos-httpclient
```

## Validation

- Validate the skill structure with:
  `python3 /home/ubuntu/.codex/skills/.system/skill-creator/scripts/quick_validate.py .claude/skills/update-std-tests`
- After updating the CSV with only passing candidates, validate the repo with:
  `cargo xtask test std`
- If the user explicitly adds failing candidates, do not promise a green validation run. Call out that the whitelist now contains known failing items.

## Resources

- `scripts/std_test_candidates.py`: audits workspace packages and rewrites `scripts/test/std_crates.csv`.
- `references/filtering.md`: explains the filtering method and the current expected baseline.
