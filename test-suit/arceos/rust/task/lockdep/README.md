# arceos-lockdep

This test app exercises lock order inversion detection for ArceOS lockdep.

## Covered cases

- `mutex-single`: single-task mutex ABBA
- `mutex-two-task`: two-task mutex ABBA
- `spin-single`: single-task spin ABBA
- `spin-two-task`: two-task spin ABBA
- `mixed-single`: single-task spin->mutex then mutex->spin
- `mixed-two-task`: two-task spin->mutex then mutex->spin
- `mixed-ms-single`: single-task mutex->spin then spin->mutex
- `mixed-ms-two-task`: two-task mutex->spin then spin->mutex

## Test modes

Default `build-*.toml` configs keep the baseline no-lockdep path while pinning
one representative `LOCKDEP_CASE` per target:

- `x86_64`: `mutex-two-task`
- `riscv64gc`: `spin-single`
- `aarch64`: `mixed-single`
- `loongarch64`: `mixed-ms-single`

Dedicated baseline configs remain available for explicit manual runs:

- `build-base-x86_64-unknown-none.toml`
- `build-base-riscv64gc-unknown-none-elf.toml`
- `build-base-aarch64-unknown-none-softfloat.toml`
- `build-base-loongarch64-unknown-none-softfloat.toml`

## Expected results

- With `lockdep` enabled, QEMU output should first print:
  `lockdep: lock order inversion detected`
- Without `lockdep`, the app should end with:
  `All tests passed!`

`cargo xtask arceos test qemu` reads the package-local
`test-suit/arceos/rust/task/lockdep/qemu-test.toml` rule file and selects the
QEMU expectation based on whether the effective feature set enables `lockdep`.

## Common commands

Baseline on x86_64:

```bash
cargo xtask arceos test qemu --only-rust --package arceos-lockdep --target x86_64-unknown-none
```

Baseline on riscv64:

```bash
cargo xtask arceos test qemu --only-rust --package arceos-lockdep --target riscv64gc-unknown-none-elf
```

Manual baseline on x86_64:

```bash
cargo xtask arceos qemu \
  --package arceos-lockdep \
  --target x86_64-unknown-none \
  --config test-suit/arceos/rust/task/lockdep/build-base-x86_64-unknown-none.toml \
  --qemu-config test-suit/arceos/rust/task/lockdep/qemu-base-x86_64.toml
```

Run a specific manual case with lockdep on x86_64:

```bash
FEATURES=lockdep LOCKDEP_CASE=mixed-ms-two-task cargo xtask arceos qemu \
  --package arceos-lockdep \
  --target x86_64-unknown-none \
  --config test-suit/arceos/rust/task/lockdep/build-x86_64-unknown-none.toml
```
