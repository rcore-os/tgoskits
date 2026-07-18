# posix-mqueue

Carpet coverage for the StarryOS POSIX message queue subsystem (`mq_open`,
`mq_unlink`, `mq_timedsend`, `mq_timedreceive`, `mq_notify`, `mq_getsetattr`
and the `/dev/mqueue` pseudo filesystem).

Run it with:

```bash
cargo xtask starry app qemu -t posix-mqueue --arch aarch64
```

(`--arch` also accepts `riscv64`, `x86_64`, `loongarch64`.)

## What it exercises

The app injects static musl binaries into the managed Alpine rootfs and runs
`/usr/bin/run-mq-tests.sh`, which drives two layers:

1. **Open POSIX Test Suite `conformance/interfaces/mq_*`** - 119 standards
   conformance cases (mq_open / mq_close / mq_unlink / mq_send / mq_receive /
   mq_timedsend / mq_timedreceive / mq_notify / mq_getattr / mq_setattr).
   These are the authoritative POSIX compliance reference. Each case is a
   self-contained `main()` returning `PTS_PASS` (0) on success; `PTS_FAIL` (1)
   and `PTS_UNRESOLVED` (2) are both real failures (`PTS_UNRESOLVED` means the
   test hit an error before reaching a verdict, which must not happen on a
   correct kernel); `PTS_UNSUPPORTED` (4) and `PTS_UNTESTED` (5) are reported
   as skips with a fixed ceiling of 10 (any new skips beyond that gate cause
   the suite to report `TEST FAILED`).

2. **Deterministic self-written carpet** (`mq_carpet`) - covers the edges the
   conformance suite under-exercises on a single-user kernel: strict priority
   ordering across and within priority, full/empty blocking with
   EAGAIN / ETIMEDOUT, EMSGSIZE / EEXIST / ENOENT / EINVAL, the
   CAP_SYS_RESOURCE ceiling bypass for a privileged (root) caller,
   mq_notify SIGEV_SIGNAL / SIGEV_NONE / SIGEV_THREAD / EBUSY, cross-process
   fork IPC, and the `/dev/mqueue/<name>`
   `QSIZE:/NOTIFY:/SIGNO:/NOTIFY_PID:` status format. It also exercises the
   full-parity behaviours: `RLIMIT_MSGQUEUE` byte accounting (a create past the
   soft limit fails with EMFILE), the writable `/proc/sys/fs/mqueue/*` tunables
   (read-back plus a lowered `msg_max` honored by the next `mq_open`), and the
   `/dev/mqueue/<name>` inode `stat` reporting the queue's real mode, creator
   uid and timestamps.

The runner prints `MQ_OK=<pass>/<total>` and `TEST PASSED` only when the
self-written carpet passes and no conformance case reports `PTS_FAIL` or
`PTS_UNRESOLVED`.

## Ground truth

Both layers are validated against the reference implementation (Linux
`ipc/mqueue.c`): the self-written carpet passes 48/48 on StarryOS. On a glibc
host it passes 47/48 - the one host miss is the embedded-slash `mq_open`
(`ok 5`), a documented glibc-vs-musl divergence (glibc rejects the interior
slash before the syscall with `EACCES`, whereas musl passes the bare name
through so the kernel returns `EINVAL`); StarryOS uses musl and returns
`EINVAL`. The Open POSIX conformance suite passes 109/119 as root (the
remaining cases are environment-specific and reported as skips, not failures).

## Not bundled: LTP

The LTP `testcases/kernel/syscalls/mq_*` cases depend on the libltp runtime
(the `tst_test` harness, `SAFE_*` wrappers, `needs_root`/`nobody`-user
assumptions). Building libltp reproducibly for the four cross targets is not
practical in this app's prebuild, so the Open POSIX conformance suite is used
as the standards-compliance authority instead. Integrating a cross-built
libltp remains open work.

## Provenance and license

The `programs/openposix/mq_*` cases are vendored from the Open POSIX Test Suite
(part of the Linux Test Project, `testcases/open_posix_testsuite/conformance/
interfaces/mq_*`) at the exact upstream revision:

- Repository: `https://github.com/linux-test-project/ltp`
- Release tag: `20260130` (commit `6a60ae592cd375f004df0694efc7d50ddae9aa5e`)
- Modifications: **none**. All 119 vendored `mq_*/*.c` files are byte-identical
  to that tag (verified with `diff` against
  `raw.githubusercontent.com/linux-test-project/ltp/20260130/...`). Adapting a
  case, if ever needed, must be recorded here as an explicit modification entry.

Each file carries its original Intel copyright header licensed under GPL-2.0,
which refers to "the COPYING file at the top level of this source tree"; that
license text and the upstream provenance are provided in
`programs/openposix/COPYING`. The GPL-2.0 notice applies only to those vendored
sources, which are kept segregated under `programs/openposix/` and merely
aggregated with (not relicensed into) this repository; the self-written
`mq_carpet.c`, the runner and the rest of the repository are covered by the
repository-root license.

## Kernel implementation

The subsystem lives in the StarryOS kernel:

- `kernel/src/ipc/mqueue.rs` - the queue object (a `FileLike` fd target), the
  global name registry, priority-ordered storage, blocking/timeout via
  `PollSet` + `timeout_at_wall`, per-user `RLIMIT_MSGQUEUE` byte accounting,
  inode timestamps, and single-shot `mq_notify` delivery for SIGEV_SIGNAL,
  SIGEV_NONE and SIGEV_THREAD (the last over a netlink cookie).
- `kernel/src/syscall/ipc/mqueue.rs` - the six syscalls and their ABI glue.
- `kernel/src/pseudofs/mqueue.rs` - the `/dev/mqueue` mqueuefs (real inode
  mode/uid/gid/timestamps per queue).
- `kernel/src/pseudofs/proc.rs` - the writable `/proc/sys/fs/mqueue/*` tunables.

Limits follow Linux defaults: `msg_default`=10, `msgsize_default`=8192,
`msg_max`=10, `msgsize_max`=8192, `queues_max`=256, with a privileged caller
bounded instead by `HARD_MSGMAX`=65536 / `HARD_MSGSIZEMAX`=16 MiB. The five
size tunables are live and adjustable through `/proc/sys/fs/mqueue/*`
(matching `ipc/mq_sysctl.c`), and each queue's bytes are charged against the
creator's `RLIMIT_MSGQUEUE` (default `MQ_BYTES_MAX`=819200), as in
`ipc/mqueue.c`.
