# StarryOS syscall boundary tests

This document records the boundary-test logic for the StarryOS `getrandom`,
`getrlimit`, and `setrlimit` syscall paths.

These tests are designed to catch implementation bugs, not just exercise the
happy path. A test suite cannot prove that a syscall is bug-free for every
future implementation, but the cases below cover the Linux-visible boundary
semantics used by applications and the error paths that are most likely to
regress in StarryOS.

## Test cases

- `test-suit/starryos/normal/test-getrandom`
- `test-suit/starryos/normal/test-prlimit64`

Run on `riscv64`:

```bash
cargo xtask starry test qemu --arch riscv64 -c test-getrandom
cargo xtask starry test qemu --arch riscv64 -c test-prlimit64
```

When running from macOS, use the project Docker environment so Linux host tools
such as `debugfs` and `qemu-riscv64-static` are available:

```bash
docker exec tgoskits-work sh -lc 'cd /workspace && cargo xtask starry test qemu --arch riscv64 -c test-getrandom'
docker exec tgoskits-work sh -lc 'cd /workspace && cargo xtask starry test qemu --arch riscv64 -c test-prlimit64'
```

## `getrandom`

The `test-getrandom` case calls `SYS_getrandom` directly through `syscall()` to
avoid hiding kernel behavior behind libc wrappers.

Covered boundaries:

- Basic successful read: non-zero buffer length returns the exact requested
  length and changes the output buffer.
- Zero-length reads: `len == 0` returns `0` and does not touch the user buffer,
  even when `buf` is `NULL` or an invalid address.
- All valid flag combinations:
  `0`, `GRND_NONBLOCK`, `GRND_RANDOM`, `GRND_RANDOM | GRND_NONBLOCK`,
  `GRND_INSECURE`, and `GRND_INSECURE | GRND_NONBLOCK`.
- Invalid flag bits: unknown bits return `EINVAL` and leave the user buffer
  unchanged.
- Mutually exclusive flags: `GRND_RANDOM | GRND_INSECURE` returns `EINVAL` and
  leaves the user buffer unchanged.
- Bad user pointers: non-zero reads to `NULL` or an invalid address return
  `EFAULT`.
- Error priority: invalid flags return `EINVAL` before the kernel attempts to
  write to a bad user pointer.
- Larger request: a 4096-byte request returns the full length and fills the
  buffer, catching implementations that only handle small reads.

## `getrlimit` and `setrlimit`

On the tested musl/riscv64 userspace, `getrlimit` and `setrlimit` use the
`prlimit64` syscall path. The `test-prlimit64` case therefore verifies both the
raw `prlimit64` behavior and the libc `getrlimit`/`setrlimit` entry points that
applications call.

Covered boundaries:

- `getrlimit(RLIMIT_NOFILE)` and `getrlimit(RLIMIT_STACK)` succeed and report
  `soft <= hard`.
- Invalid resource IDs return `EINVAL` for both `getrlimit` and `setrlimit`.
- Raw `prlimit64` bad user pointers return `EFAULT` for both `old_limit` writes
  and `new_limit` reads.
- `getrlimit`/`setrlimit` with a `NULL` libc wrapper pointer are documented as
  a no-op in this test because the current Linux/glibc path maps them to
  `prlimit64(..., NULL, NULL)` and returns success. The test verifies that this
  no-op does not change the stored limits.
- `setrlimit` rejects `rlim_cur > rlim_max` with `EINVAL`.
- Lowering only the soft limit succeeds, and a following `getrlimit` observes
  the new soft limit while the hard limit remains unchanged.
- Setting `rlim_cur == rlim_max` succeeds and is visible through `getrlimit`.
- Lowering the hard limit succeeds, and both soft and hard values observed by
  `getrlimit` match the requested values.
- Restoring a previously lowered hard limit succeeds and is visible through
  `getrlimit`; this catches silent no-op bugs where the syscall returns success
  without applying the new hard limit.
- The existing `prlimit64` checks also cover reading old limits, lowering hard
  limits, raising hard limits, setting and getting atomically, `soft > hard`,
  invalid resources, and `getrusage` sanity checks.

## Remaining assumptions

- These tests validate Linux-compatible user-visible behavior for the current
  StarryOS `riscv64` QEMU environment.
- Passing the tests means the covered syscall boundaries match expectations; it
  does not replace code review, fuzzing, or tests on other architectures.
