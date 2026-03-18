# Filtering Std Test Candidates

## Summary

Use `cargo metadata --no-deps` to enumerate workspace packages, compare them against `scripts/test/std_crates.csv`, then classify the missing packages by full host `cargo test -p <package>` behavior.

## Candidate Source

- Source packages from the current workspace only.
- Treat `scripts/test/std_crates.csv` as the authoritative existing whitelist.
- Ignore blank CSV lines; require a single `package` header.

## Inclusion Rules

- Include `lib` packages in the audit candidate set.
- Include examples/bin-only packages in the audit candidate set.
- Use the full `cargo test -p <package>` result, not `--no-run`.

## Default Exclusions

- Exclude `tg-xtask` because it is repository tooling.
- Exclude `axlibc` because it is `staticlib`-only.
- Exclude `arm_vcpu` and `riscv_vcpu` because they are architecture-specific host-incompatible packages.
- Exclude `axvisor` because it is a bare-metal application package.
- Exclude failures that clearly indicate host incompatibility:
  - `invalid register` - inline assembly incompatible with host
  - `undefined symbol: main` - missing host entrypoint

## Exclusion Reasoning

Packages are excluded to avoid false negatives in the test suite:

| Category | Reason | Examples |
|----------|--------|----------|
| Tooling | Not part of the std test suite | `tg-xtask` |
| Architecture-specific | Won't compile on host | `arm_vcpu`, `riscv_vcpu` |
| Build artifact | Not a testable package | `axlibc` (staticlib-only) |
| Bare-metal | Requires custom runtime | `axvisor` |
| Host-incompatible patterns | Test would always fail on host | Invalid register errors |

## Current Expected Behavior

When running the audit script, expect these categories:

- **Passing candidates**: Packages that pass `cargo test -p <package>` on the host
- **Failing candidates**: Packages that fail but might be valid (e.g., missing dependencies)
- **Excluded candidates**: Packages that should never be in the whitelist

Re-run the audit script whenever:
- Workspace membership changes
- Target kinds are modified
- Host test behavior changes
- Dependencies are updated

This baseline reflects the filtering logic, not a fixed allowlist. The actual candidates will vary based on the workspace state.
