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

Default `build-*.toml` configs are now used for automated lockdep smoke.
They enable `lockdep` and pin one representative `LOCKDEP_CASE` per target:

- `x86_64`: `mutex-two-task`
- `riscv64gc`: `spin-single`
- `aarch64`: `mixed-single`
- `loongarch64`: `mixed-ms-single`

Manual baseline configs keep the old no-lockdep path:

- `build-base-x86_64-unknown-none.toml`
- `build-base-riscv64gc-unknown-none-elf.toml`
- `build-base-aarch64-unknown-none-softfloat.toml`
- `build-base-loongarch64-unknown-none-softfloat.toml`

## Expected results

- With `lockdep` enabled, QEMU output should first print:
  `lockdep: lock order inversion detected`
- Without `lockdep`, the app should end with:
  `All tests passed!`

The QEMU configs accept both outputs in `success_regex`, but the automated smoke
path is expected to hit the lockdep text first.

## Common commands

Automated smoke on x86_64:

```bash
cargo xtask arceos test qemu --only-rust --package arceos-lockdep --target x86_64-unknown-none
```

Automated smoke on riscv64:

```bash
cargo xtask arceos test qemu --only-rust --package arceos-lockdep --target riscv64gc-unknown-none-elf
```

Manual baseline on x86_64:

```bash
cargo xtask arceos qemu \
  --package arceos-lockdep \
  --target x86_64-unknown-none \
  --config test-suit/arceos/rust/task/lockdep/build-base-x86_64-unknown-none.toml
```

Run a specific manual case with lockdep on x86_64:

```bash
LOCKDEP_CASE=mixed-ms-two-task cargo xtask arceos qemu \
  --package arceos-lockdep \
  --target x86_64-unknown-none \
  --config test-suit/arceos/rust/task/lockdep/build-lockdep-x86_64-unknown-none.toml
```

## Current status

- ArceOS lockdep smoke is working on `x86_64` and `riscv64`.
- `FEATURES=lockdep cargo xtask starry test qemu --target riscv64gc-unknown-none-elf`
  currently fails in Starry C-case prebuild/link steps for some cases
  (`crtbeginS.o`, `-lgcc`), which is separate from the lockdep output path.
