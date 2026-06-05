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
- `vfs-cache-single`: single-task `axfs-ng-vfs` dentry user-data/cache ABBA

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

## Expected Results

- With `lockdep` enabled, QEMU output should first print:
  `lockdep: lock order inversion detected`
- Without `lockdep`, the app should end with:
  `All tests passed!`
- In the `lockdep`-enabled path, the inversion banner is the success signal.
  Lockdep then emits its report through the fatal path and shuts the system down
  without entering the panic handler.
- If `lockdep` is enabled but no inversion is reported, the app panics with:
  `lockdep did not report an expected lock order inversion`

`cargo xtask arceos test qemu` uses `qemu-{arch}.toml`, whose success patterns
accept either the baseline completion line or the lockdep inversion banner.
Panic output remains a failure signal, so the lockdep-enabled path still fails
if no inversion is reported before the fallback panic.

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
  kind=spin lock class=9 subclass=0 addr=0xffffffc08029f9d0 acquire_at=test-suit/arceos/rust/task/lockdep/src/main.rs:127:16
conflicting held lock:
  class=10 subclass=0 addr=0xffffffc08029f9f0 acquired_at=test-suit/arceos/rust/task/lockdep/src/main.rs:125:27
held stack:
  [0] top: class=10 subclass=0 addr=0xffffffc08029f9f0 acquired_at=test-suit/arceos/rust/task/lockdep/src/main.rs:125:27

lockdep fatal violation
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
requested:
  kind=mutex class=12 subclass=0 addr=0xffffffc08030eef0 acquire_at=os/arceos/modules/axnet/src/smoltcp_impl/mod.rs:174:35
conflicting held lock:
  class=9 subclass=0 addr=0xffffffc08030e300 acquired_at=os/arceos/modules/axnet/src/smoltcp_impl/mod.rs:173:36
held stack:
  [0] held: class=13 subclass=0 addr=0xffffffc08030d120 acquired_at=os/arceos/modules/axnet/src/smoltcp_impl/mod.rs:172:32
  [1] top: class=9 subclass=0 addr=0xffffffc08030e300 acquired_at=os/arceos/modules/axnet/src/smoltcp_impl/mod.rs:173:36

lockdep fatal violation
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
  --qemu-config test-suit/arceos/rust/task/lockdep/qemu-x86_64.toml
```

Run a specific manual case with lockdep on x86_64:

```bash
FEATURES=lockdep LOCKDEP_CASE=mixed-ms-two-task cargo xtask arceos qemu \
  --package arceos-lockdep \
  --target x86_64-unknown-none \
  --config test-suit/arceos/rust/task/lockdep/build-x86_64-unknown-none.toml \
  --qemu-config test-suit/arceos/rust/task/lockdep/qemu-x86_64.toml
```

Run the VFS cache visibility case with lockdep on x86_64:

```bash
FEATURES=lockdep cargo xtask arceos qemu \
  --package arceos-lockdep \
  --target x86_64-unknown-none \
  --config test-suit/arceos/rust/task/lockdep/build-vfs-cache-x86_64-unknown-none.toml \
  --qemu-config test-suit/arceos/rust/task/lockdep/qemu-x86_64.toml
```
