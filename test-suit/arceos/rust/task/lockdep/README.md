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
- In the `lockdep`-enabled path, the inversion banner is the success signal.
  The app may panic immediately afterwards, which is expected for this
  regression test and does not count as a failure.

`cargo xtask arceos test qemu` reads the package-local
`test-suit/arceos/rust/task/lockdep/qemu-test.toml` rule file and selects the
QEMU expectation based on whether the effective feature set enables `lockdep`.
Direct `cargo xtask arceos qemu` runs do not consult `qemu-test.toml`; when you
run the app manually, pass the intended `--qemu-config` explicitly.

## Violation Examples

Enable `lockdep` on the Rust regression app to confirm that the dedicated
`arceos-lockdep` cases can trigger an inversion report:

```bash
FEATURES=lockdep cargo xtask arceos test qemu --only-rust \
  --package arceos-lockdep \
  --target riscv64gc-unknown-none-elf
```

Typical output looks like:

```text
lockdep: lock order inversion detected
requested:
  kind=spin lock id=23 class=9 addr=0xffffffc08029f9d0 acquire_at=test-suit/arceos/rust/task/lockdep/src/main.rs:127:16
conflicting held lock:
  id=24 class=10 addr=0xffffffc08029f9f0 acquired_at=test-suit/arceos/rust/task/lockdep/src/main.rs:125:27
held stack:
  [0] top: id=24 class=10 addr=0xffffffc08029f9f0 acquired_at=test-suit/arceos/rust/task/lockdep/src/main.rs:125:27
```

Enable `lockdep` on the ArceOS C-test batch to look for system-level lock order
problems outside the dedicated regression app. In the current workflow, this
batch is expected to expose the `httpclient` violation:

```bash
FEATURES=lockdep cargo xtask arceos test qemu --only-c \
  --target riscv64gc-unknown-none-elf
```

Typical output from the current `httpclient` violation looks like:

```text
Hello, ArceOS C HTTP client!
lockdep: lock order inversion detected
panicked at components/lockdep/src/state.rs:639:13:
lockdep: lock order inversion detected
requested:
  kind=mutex id=16 class=12 addr=0xffffffc08030eef0 acquire_at=os/arceos/modules/axnet/src/smoltcp_impl/mod.rs:174:35
conflicting held lock:
  id=12 class=9 addr=0xffffffc08030e300 acquired_at=os/arceos/modules/axnet/src/smoltcp_impl/mod.rs:173:36
held stack:
  [0] held: id=18 class=13 addr=0xffffffc08030d120 acquired_at=os/arceos/modules/axnet/src/smoltcp_impl/mod.rs:172:32
  [1] top: id=12 class=9 addr=0xffffffc08030e300 acquired_at=os/arceos/modules/axnet/src/smoltcp_impl/mod.rs:173:36
```

## Common commands

Baseline test on x86_64:

```bash
cargo xtask arceos test qemu --only-rust --package arceos-lockdep --target x86_64-unknown-none
```

Baseline test on riscv64:

```bash
cargo xtask arceos test qemu --only-rust --package arceos-lockdep --target riscv64gc-unknown-none-elf
```

Lockdep-enabled test on riscv64:

```bash
FEATURES=lockdep cargo xtask arceos test qemu --only-rust \
  --package arceos-lockdep \
  --target riscv64gc-unknown-none-elf
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
  --config test-suit/arceos/rust/task/lockdep/build-x86_64-unknown-none.toml \
  --qemu-config test-suit/arceos/rust/task/lockdep/qemu-x86_64.toml
```
