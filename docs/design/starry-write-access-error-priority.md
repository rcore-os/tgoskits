# Starry write-access error priority

Status: implemented and verified; frozen KernDiff scenario and all four
post-fix QEMU cases passed
Target reference: Linux 6.18, commit
[`7d0a66e4bb9081d75c82ec4957c50034cb0ea449`](https://github.com/torvalds/linux/commit/7d0a66e4bb9081d75c82ec4957c50034cb0ea449)

## Problem and success criteria

A frozen KernDiff scenario combines a pipe read endpoint with an invalid write
buffer. Linux 6.18 returns `EBADF`, while Starry imports the user buffer first
and returns `EFAULT`. The same ordering defect affects scalar, vectored, and
positioned writes.

The affected users are applications that depend on Linux syscall errno
semantics and KernDiff runs that compare Starry with the fixed Linux reference.
The change is complete when:

- `write` and stream-style vectored writes reject a descriptor that is not open
  for writing before importing user buffers;
- positioned vectored writes reject nonseekable objects with `ESPIPE` and
  read-only seekable files with `EBADF` before importing `iovec`;
- Eventfd length, Memfd seal, flags, offset, and zero-length priorities remain
  unchanged;
- the direct-syscall QEMU regression, related QEMU cases, and the frozen
  KernDiff scenario pass.

Keeping the old order makes Linux-compatible software observe a different
errno whenever descriptor state and an invalid user pointer are both present.
This is an ABI defect rather than a test-only discrepancy.

## Linux 6.18 reference semantics

The public contract says `EBADF` applies when a descriptor is not open for
writing and `EFAULT` applies to an inaccessible buffer
([`write(2)`](https://man7.org/linux/man-pages/man2/write.2.html)). The man page
does not define their priority, so the fixed Linux source is authoritative:

- [`vfs_write`](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/read_write.c#L666-L675)
  checks `FMODE_WRITE` before `access_ok`;
- [`vfs_writev`](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/read_write.c#L1028-L1045)
  checks `FMODE_WRITE` before `import_iovec`;
- [`do_pwritev`](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/read_write.c#L1141-L1154)
  returns `ESPIPE` unless the descriptor supports positioned writes, then calls
  `vfs_writev`, which distinguishes a read-only file with `EBADF`;
- [`pwritev2`](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/read_write.c#L1194-L1211)
  routes offset `-1` through stream `writev` and other offsets through
  positioned `pwritev`.

The same six inputs were run directly on host Linux
`6.18.33.2-microsoft-standard-WSL2`; the complete test reported 76 passes and
zero failures.

## Starry call chain and affected interfaces

The dispatch path is:

1. `os/StarryOS/kernel/src/syscall/mod.rs` selects `sys_write`, `sys_writev`,
   `sys_pwritev`, `sys_pwritev2`, or `sys_io_uring_enter`.
2. `os/StarryOS/kernel/src/syscall/fs/io.rs` resolves the descriptor, validates
   operation-specific arguments, imports user memory, applies Memfd seal
   policy, and executes the write.
3. `os/StarryOS/kernel/src/file/mod.rs` exposes the `FileLike` capability
   boundary; `File`, `Pipe`, `Directory`, `Memfd`, and file-backed wrappers
   implement the relevant policy.
4. `os/StarryOS/kernel/src/syscall/fs/io_uring.rs` routes
   `IORING_OP_WRITEV` through `sys_pwritev2` and stream `IORING_OP_WRITE`
   through `sys_write`, so `io_uring_enter` observes the same validation.

The standard mapping required for the shared helper is:

| Syscall | Conclusion | Reference | Basis |
| --- | --- | --- | --- |
| `write` | Aligned after this change | [`write(2)`](https://man7.org/linux/man-pages/man2/write.2.html), [Linux `vfs_write`](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/read_write.c#L666-L675) | Write mode is checked before scalar user-buffer access. |
| `writev` | Aligned after this change | [`writev(2)`](https://man7.org/linux/man-pages/man2/readv.2.html), [Linux `vfs_writev`](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/read_write.c#L1028-L1045) | Write mode is checked before importing the `iovec` array. |
| `pwritev` | Aligned after this change | [`pwritev(2)`](https://man7.org/linux/man-pages/man2/readv.2.html), [Linux `do_pwritev`](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/read_write.c#L1141-L1154) | Nonseekable descriptors return `ESPIPE`; a read-only seekable file reaches the write-mode check and returns `EBADF`. |
| `pwritev2` | Aligned after this change | [`pwritev2(2)`](https://man7.org/linux/man-pages/man2/readv.2.html), [Linux `pwritev2` routing](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/read_write.c#L1202-L1211) | Offset `-1` uses stream ordering; positioned offsets use `pwritev` ordering. |
| `io_uring_enter` | Indirectly aligned after this change | [`io_uring_enter(2)`](https://man7.org/linux/man-pages/man2/io_uring_enter.2.html), [Linux write-mode boundary](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/read_write.c#L1028-L1045) | Starry executes write SQEs through the same corrected syscall helpers and reports their errno in CQEs. |

No ABI layout, syscall number, credential, namespace, shared-resource, blocking,
signal, or restart behavior changes. The new check reads immutable/open-file
description state and does not introduce a new lock or state transition.

## Design and ordering

`FileLike::validate_write_access()` is an internal capability preflight. Its
default accepts bidirectional anonymous objects. Implementations with a
direction or open-mode restriction override it:

- `File` checks only the underlying `FileFlags::WRITE` open mode and rejects
  path-only handles;
- `Pipe` accepts only its write endpoint;
- `Directory` always returns `EBADF`;
- `Memfd` and `MountTableFile` delegate to their wrapped `File`;
- Memfd seals are deliberately not part of the access preflight.

The syscall order is:

| Path | Validation order |
| --- | --- |
| `write` | fd → write access → object length rule → user buffer → Memfd seal → write |
| `writev` | fd → write access → aggregate/object length rule → user segments → Memfd seal → write |
| `pwritev` / positioned `pwritev2` | flags/offset → fd and positioned capability → write access → user segments → Memfd seal → write-at |
| `pwritev2(offset=-1)` | flags/offset → fd → write access → object length rule → user segments → Memfd seal → stream write |

This preserves the following established behavior:

- Eventfd rejects a non-8-byte scalar write or vectored total with `EINVAL`
  before touching a bad segment address. A bad `iovec` array still produces
  `EFAULT` because its entries are required to compute the total.
- A sealed Memfd with an invalid user address returns `EFAULT` before `EPERM`.
  The access hook checks only open mode; seal checks remain after user-region
  validation.
- Unsupported `pwritev2` flags and invalid negative offsets retain their
  existing priority.
- Zero-length operations retain their existing object-specific result; in
  particular, a zero-length write does not synthesize a Memfd seal failure.

## Alternatives

| Alternative | Decision |
| --- | --- |
| Special-case Pipe in `writev` | Rejected. It would fix only the frozen input and leave scalar writes, read-only regular files, positioned writes, and `io_uring` inconsistent. |
| Import user memory and remap `EFAULT` afterward | Rejected. It hides the real validation order, can fault or allocate unnecessarily, and cannot safely infer which competing error should win. |
| Infer access generically from `open_flags()` | Rejected. Anonymous objects do not all encode direction with regular-file flags, while Pipe direction is owned by the endpoint object. |
| Add an internal `FileLike` capability preflight | Chosen. The object that owns the write invariant reports it once, and all shared write entry points use the same boundary. |

## Non-goals and rollback

- The mirrored `readv(pipe write endpoint, bad iovec)` discrepancy is not part
  of this change and requires its own read-access analysis and regression.
- KernDiff, its adapter/oracle, and the frozen session are not modified.
- This change does not add support for new `RWF_*` flags or alter Memfd seal
  semantics.
- It does not redesign all `FileLike` operation capabilities; only the shared
  write-access ordering required by current callers is added.

The change is stateless and has no migration requirement. Reverting the code,
test, and this document restores the previous behavior.

## Validation record

### Regression effectiveness

Command:

```bash
cargo xtask starry test qemu --arch x86_64 \
  -c qemu/system/syscall-test-vectored-io
```

Before the fix on 2026-08-16, the user-reported run produced 70 passes and six
failures. Every new case returned `EFAULT` instead of the expected result:

| Scenario | Linux / expected | Pre-fix Starry |
| --- | --- | --- |
| Pipe read endpoint + bad `write` buffer | `EBADF` | `EFAULT` |
| Pipe read endpoint + bad `writev` array | `EBADF` | `EFAULT` |
| `O_RDONLY` file + bad `writev` array | `EBADF` | `EFAULT` |
| `O_RDONLY` file + bad `pwritev` array | `EBADF` | `EFAULT` |
| `pwritev2(offset=-1)` on Pipe read endpoint + bad array | `EBADF` | `EFAULT` |
| Positioned `pwritev2` on Pipe + bad array | `ESPIPE` | `EFAULT` |

The grouped runner propagated the failure with
`STARRY_GROUPED_TEST_FAILED` and a nonzero xtask result, confirming the new
coverage is discovered and cannot silently pass.

### Completed local post-fix validation

Run on 2026-08-16:

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | Passed. |
| `cargo xtask clippy --package starry-kernel` | Passed all 26 configurations with warnings denied. A SHA-256-verified temporary AIC8800 firmware cache was supplied to avoid a stalled GitHub Raw request in the `sg2002-wifi` build script. |
| `git diff --check` | Passed. |
| Host compile and execution of `syscall-test-vectored-io` | Passed: 76 passes, zero failures on Linux `6.18.33.2-microsoft-standard-WSL2`. |

### Completed frozen KernDiff validation

On 2026-08-16, the user ran the frozen minimal scenario against this Starry
workspace. The guest reported `exit_code=0` and confirmed that coverage was
triggered. KernDiff completed the single execution with:

```text
[kerndiff] command=run status=match exit=0 executions=1 continued=false reused=false session=/home/cloud/gitArceOS/KernDiff/.kerndiff/sessions/run-scenario-20260816T084034Z-6cde9255
```

This confirms the original Linux `EBADF` versus Starry `EFAULT` fingerprint no
longer differs in the frozen scenario.

### Completed post-fix QEMU validation

On 2026-08-16, the user ran the direct regression after the fix:

| Case | Result |
| --- | --- |
| `qemu/system/syscall-test-vectored-io` | Passed: 76 passes, zero failures; `STARRY_GROUPED_TESTS_PASSED`; xtask summary 1/1. |
| `qemu/system/syscall-test-eventfd2` | Passed: 94 passes, zero failures; `STARRY_GROUPED_TESTS_PASSED`; xtask summary 1/1. |
| `qemu/system/syscall-test-modern-fd-family` | Passed: 245 passes, zero failures; `STARRY_GROUPED_TESTS_PASSED`; xtask summary 1/1. |
| `qemu/system/syscall-test-io-uring` | Passed: 50 passes, zero failures; `STARRY_GROUPED_TESTS_PASSED`; xtask summary 1/1. |

All four QEMU commands reported their normal all-passed marker and exited
successfully. Together with the frozen KernDiff match, this completes the
required runtime validation.
