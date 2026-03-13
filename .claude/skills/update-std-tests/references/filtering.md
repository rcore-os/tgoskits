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
- Exclude future failures that clearly indicate host incompatibility, such as `invalid register` or `undefined symbol: main`.

## Current Repo Baseline

Passing candidates not currently in the CSV:

- `arceos-helloworld`
- `arceos-helloworld-myplat`
- `arceos-httpclient`
- `arceos-httpserver`
- `arceos-shell`

Failing host/std candidates not currently in the CSV:

- `arceos_posix_api`
- `axdma`
- `axmm`
- `axfs`
- `axfs-ng`
- `axnet-ng`
- `axstd`

Excluded candidates not currently in the CSV:

- `tg-xtask`
- `axlibc`
- `arm_vcpu`
- `riscv_vcpu`
- `axvisor`

Re-run the audit script whenever the workspace membership, target kinds, or host test behavior changes. Treat this baseline as expected current output, not a permanent allowlist.
