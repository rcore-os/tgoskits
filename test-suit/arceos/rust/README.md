# ArceOS Rust Test Suite

`arceos-test-suit` is the single Rust QEMU test entry for ArceOS. Individual
tests are crate modules gated by Cargo features, and the `all` feature enables
the runnable regression set in a deterministic order.

Run all Rust tests in one QEMU boot:

```bash
cargo xtask arceos test qemu --test-group rust --target x86_64-unknown-none
```

Run one test feature:

```bash
cargo xtask arceos test qemu --test-group rust --test-case task-yield --target x86_64-unknown-none
```

The old per-test Rust crates and `--package` selection are intentionally
removed. Use `--test-case` with the feature name printed by `--list`.
