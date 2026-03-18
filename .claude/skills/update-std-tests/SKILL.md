---
name: update-std-tests
description: Audit and update `scripts/test/std_crates.csv` for this ArceOS/StarryOS workspace. Use this skill whenever the user mentions std tests, whitelist, cargo test validation, wants to check which packages pass host tests, refreshes the test suite, or asks about adding new packages to the test CSV. This is the primary way to manage the std test candidates list.
---

# Update Std Tests

This skill manages the std test whitelist (`scripts/test/std_crates.csv`) by auditing workspace packages against host `cargo test` results.

## Workflow

1. **Run audit** to identify candidates not in the whitelist
2. **Ask about passing candidates** first (recommended to add)
3. **Ask about failing candidates** second (opt-in, these currently fail)
4. **Apply only confirmed packages** after user approval
5. **Optional validation** with `cargo xtask test std` if only passing packages added

## Commands

The script is located at `<skill-path>/scripts/std_test_candidates.py`.

**Audit (Markdown output):**
```bash
python3 scripts/std_test_candidates.py audit --repo-root /path/to/repo --format markdown
```

**Audit (JSON output):**
```bash
python3 scripts/std_test_candidates.py audit --repo-root /path/to/repo --format json
```

**Apply packages to CSV:**
```bash
python3 scripts/std_test_candidates.py apply --repo-root /path/to/repo --packages pkg1 pkg2 pkg3
```

**Dry-run (preview changes without applying):**
```bash
python3 scripts/std_test_candidates.py apply --repo-root /path/to/repo --packages pkg1 pkg2 --dry-run
```

## How to Ask User

Always get confirmation before applying changes. For passing candidates, ask "Add all passing packages?" (yes/no). For failing candidates, ask "Add failing packages? Options: `all`, `ignore`, or comma-separated names".

## Filtering Policy

- **Include**: `lib` packages, `bin-only` examples
- **Exclude by name**: `tg-xtask`, `axlibc`, `arm_vcpu`, `riscv_vcpu`, `axvisor`
- **Exclude by failure pattern**: `invalid register`, `undefined symbol: main` (host-incompatible)
- **Test method**: Full `cargo test -p <package>`, not `--no-run`

See `references/filtering.md` for detailed explanation of the filtering logic.

## Output Format

Show candidates in this order with clear visual separation:

```
## ✅ Passing candidates (N)
- `package-name` (type) - path - passes cargo test

## ❌ Failing candidates (N)
- `package-name` (type) - path - error message

## ⏭️ Excluded candidates (N)
- `package-name` (type) - path - exclusion reason
```

## Validation

After updating with only passing packages, suggest running:
```bash
cargo xtask test std
```

If failing packages were added, explicitly warn that the whitelist contains known failing items and validation may not pass.

## Resources

- `scripts/std_test_candidates.py`: Main audit and apply script
- `references/filtering.md`: Detailed filtering policy documentation
