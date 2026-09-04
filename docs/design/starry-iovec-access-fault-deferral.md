# Starry iovec access-fault deferral

Status: implemented and verified
Target reference: Linux commit
[`a2cf4ef33184df0ae9e1a2b05b550133dde1698c`](https://github.com/torvalds/linux/commit/a2cf4ef33184df0ae9e1a2b05b550133dde1698c)

## Problem and success criteria

Starry imported every nonempty `iovec` segment through the general mapped-user
memory check. That check rejects addresses below Starry's mapping base
`0x1000`, so an unmapped but numerically in-range address such as `0x1` caused
`readv` to return `EFAULT` before the file operation ran.

Linux separates these boundaries. Its iovec import verifies that a segment is
inside the numerical user address limit, but page accessibility is tested only
when data is copied. Consequently an empty `O_NONBLOCK` pipe whose writer is
still open returns `EAGAIN` without touching an invalid destination. Once the
pipe has data, the same destination returns `EFAULT`, and the failed copy does
not consume that data.

The change is complete when:

- `IoVectorBuf` accepts nonempty segments below Starry's mapping base when the
  complete numerical range remains below the architecture's user-space limit;
- a segment at or beyond that limit, a range crossing the limit, and arithmetic
  overflow still produce `EFAULT`;
- iov count, negative segment length, aggregate length, zero-length segment,
  and partial-transfer behavior remain unchanged;
- the raw `readv` regression fails before the change and passes on x86_64
  StarryOS and the host Linux reference afterward.

KernDiff, the global mapped-memory validator, user-space layout constants, and
the pipe state machine are outside this change.

## Linux reference and Starry boundary

At the fixed target commit:

- [`vfs_readv`](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/fs/read_write.c#L991-L1026)
  checks descriptor read capability and then imports the vector;
- [`import_iovec`](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/lib/iov_iter.c#L1342-L1443)
  copies the vector array, retains count and length validation, and applies
  `access_ok` to each segment without requiring mapped pages;
- [`anon_pipe_read`](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/fs/pipe.c#L361-L491)
  returns `EAGAIN` for an empty nonblocking pipe before copying to the iterator,
  while a short copy returns `EFAULT` before advancing the pipe buffer.

Starry keeps the same two-stage structure locally. `IoVectorBuf::new` still
reads the userspace vector records and validates their structural invariants.
Its new private range helper performs only the user-limit check using
subtraction, avoiding `start + len` overflow. `IoVectorBufIo` continues to call
`vm_read_slice` or `vm_write_slice` when a file or socket actually transfers
data, so inaccessible mappings still produce `EFAULT` at the access point.

Zero-length segments continue to bypass segment-address validation. This
preserves the existing zero-length behavior rather than broadening this change
into a general user-pointer policy update.

## Shared callers and compatibility risk

`IoVectorBuf` is shared, so the review scope is wider than the motivating
`readv` case:

| Syscall | Conclusion | Reference | Basis |
| --- | --- | --- | --- |
| `readv` | Aligned by this change | [`readv(2)`](https://man7.org/linux/man-pages/man2/readv.2.html), [Linux `vfs_readv` and import](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/fs/read_write.c#L991-L1026) | A valid vector record with an in-range unmapped base reaches the underlying read before the mapping fault. |
| `preadv` | Uses the corrected boundary | [`preadv(2)`](https://man7.org/linux/man-pages/man2/readv.2.html), [Linux `import_iovec`](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/lib/iov_iter.c#L1342-L1443) | Starry routes it through `sys_preadv2`, which constructs the shared iterator. |
| `preadv2` | Uses the corrected boundary | [`preadv2(2)`](https://man7.org/linux/man-pages/man2/readv.2.html), [Linux `import_iovec`](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/lib/iov_iter.c#L1342-L1443) | Both stream and positioned read routes use `IoVectorBuf`. |
| `writev` | No intended ordering change | [`writev(2)`](https://man7.org/linux/man-pages/man2/readv.2.html), [Linux `import_iovec`](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/lib/iov_iter.c#L1342-L1443) | Starry explicitly validates readable user segments before the shared iterator copies them. |
| `pwritev` | No intended ordering change | [`pwritev(2)`](https://man7.org/linux/man-pages/man2/readv.2.html), [Linux `import_iovec`](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/lib/iov_iter.c#L1342-L1443) | It shares the prevalidated `pwritev2` copy path. |
| `pwritev2` | No intended ordering change | [`pwritev2(2)`](https://man7.org/linux/man-pages/man2/readv.2.html), [Linux `import_iovec`](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/lib/iov_iter.c#L1342-L1443) | Its stream and positioned write paths validate mappings before copying. |
| `io_uring_enter` | Indirectly uses the corrected boundary | [`io_uring_enter(2)`](https://man7.org/linux/man-pages/man2/io_uring_enter.2.html), [Linux `import_iovec`](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/lib/iov_iter.c#L1342-L1443) | Starry routes `IORING_OP_READV` through `sys_preadv2`; completion reports that helper's result. |
| `sendmsg` | Uses Linux-style segment import | [`sendmsg(2)`](https://man7.org/linux/man-pages/man2/sendmsg.2.html), [Linux message iovec import](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/net/socket.c#L2606-L2624) | The outer message and vector records are imported immediately, while an in-range segment faults only if socket transmission reads it. |
| `recvmsg` | Uses Linux-style segment import | [`recvmsg(2)`](https://man7.org/linux/man-pages/man2/recvmsg.2.html), [Linux receive routing](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/net/socket.c#L2934-L2979) | Destination mapping faults are deferred until received data is copied. |
| `sendmmsg` | Uses Linux-style segment import per message | [`sendmmsg(2)`](https://man7.org/linux/man-pages/man2/sendmmsg.2.html), [Linux send batch routing](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/net/socket.c#L2782-L2848) | Each Starry message constructs the shared iterator before attempting its send. |
| `recvmmsg` | Uses Linux-style segment import per message | [`recvmmsg(2)`](https://man7.org/linux/man-pages/man2/recvmmsg.2.html), [Linux receive batch routing](https://github.com/torvalds/linux/blob/a2cf4ef33184df0ae9e1a2b05b550133dde1698c/net/socket.c#L2992-L3097) | Each Starry destination iterator defers mapping access until a datagram is copied. |

The compatibility risk is that socket and positioned-I/O callers can now
surface an fd, readiness, or protocol error before `EFAULT` for a low unmapped
segment. That is the intended consequence of matching Linux's `access_ok`
import boundary, but this change does not claim that every unrelated message
argument or socket error has complete Linux priority parity. The outer vector
array remains an immediate userspace read, addresses above the user limit still
fail at import, and payload access remains guarded by the VM copy functions.

## Alternatives and rollback

| Alternative | Decision |
| --- | --- |
| Special-case `readv` or Pipe | Rejected. The fault timing belongs to the shared imported iterator, and a syscall/file-type branch would leave other consumers inconsistent. |
| Lower `USER_SPACE_BASE` or loosen global `check_access` | Rejected. Those boundaries define actual Starry mappings and many unrelated user-memory APIs. |
| Remove segment validation entirely | Rejected. Linux still rejects ranges outside its user address limit during import. |
| Add a private user-limit range check to `IoVectorBuf` | Chosen. It changes only imported-vector fault timing and keeps actual VM access authoritative. |

The change is stateless and requires no migration. Reverting the helper, test,
and this record restores the old eager-fault behavior.

## Validation record

Before the implementation, the x86_64 grouped regression reported 81 passes
and one failure. The empty nonblocking pipe returned `EFAULT(14)` instead of
`EAGAIN(11)`; the populated-pipe `EFAULT` and no-consumption checks passed. The
runner emitted `STARRY_GROUPED_TEST_FAILED` and returned nonzero, proving that
the new case is discovered and can fail the outer command.

Completed validation on 2026-08-16:

| Command or check | Result |
| --- | --- |
| Host CMake build and execution on Linux `6.18.33.2-microsoft-standard-WSL2` | Passed: 82 passes, zero failures. |
| `cargo fmt` | Passed using the pinned repository toolchain. |
| `cargo xtask clippy --package starry-kernel` | Passed all 26 configurations with warnings denied. |
| `git diff --check` | Passed. |
| `cargo xtask starry test qemu --arch x86_64 -c qemu/syscall-test-vectored-io` | Passed: 82 passes, zero failures; `STARRY_GROUPED_TESTS_PASSED`; xtask summary 1/1. |
