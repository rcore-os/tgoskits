# StarryOS realtime clock setting

## Background

StarryOS initializes `CLOCK_REALTIME` from the platform epoch and an immutable
monotonic counter. Before this change it had no syscall that could correct the
wall clock after boot. On an Orange Pi 5 Plus without a usable persisted RTC,
the clock therefore started near 1970. `date -s` called `clock_settime(2)` and
received `ENOSYS`.

This blocks the BuildStorm workflow in
[`testsuits-for-oskernel`](https://github.com/oscomp/testsuits-for-oskernel):
Linux can prepare a native `tg-xtask` and source tree on the shared rootfs, but
Cargo running after booting StarryOS needs a credible wall clock for source and
output timestamps.

### Pre-change dispatch evidence

This was a syscall-dispatch gap rather than a platform RTC read failure. On the
base revision, Starry's
[`time` dispatch group](https://github.com/rcore-os/tgoskits/blob/7c5bbd13320bae42110f83e9418b4167e4dd7943/os/StarryOS/kernel/src/syscall/mod.rs#L883-L893)
contained the realtime readers but neither `clock_settime` nor
`settimeofday`. Its
[`time.rs`](https://github.com/rcore-os/tgoskits/blob/7c5bbd13320bae42110f83e9418b4167e4dd7943/os/StarryOS/kernel/src/syscall/time.rs#L1-L91)
likewise contained no setter. Both syscall numbers are valid entries in the
`syscalls` crate, so they reached the dispatch match and fell through to the
[`Unimplemented syscall` fallback](https://github.com/rcore-os/tgoskits/blob/7c5bbd13320bae42110f83e9418b4167e4dd7943/os/StarryOS/kernel/src/syscall/mod.rs#L1061-L1065),
which maps `StarryError::Unsupported` to Linux `ENOSYS`.

The board reproduced that path for both calls: BusyBox `date -s` first reached
`clock_settime`, then a direct syscall diagnostic reached the obsolete
`settimeofday` entry. BuildStorm's actual date-setting implementation is
[`clock_settime(CLOCK_REALTIME, ...)`](https://github.com/oscomp/testsuits-for-oskernel/blob/6852e65d1cb570d9d98a3a6511e81f1e3999f7b8/busybox/coreutils/date.c#L289-L292).
The direct `settimeofday` call was only a fallback diagnostic after the real
path had failed; its timezone and first-call RTC-warp ABI remain outside this
focused change.

## Goals and acceptance criteria

- Implement the Linux `clock_settime(CLOCK_REALTIME, ...)` ABI used by
  `date -s`.
- Make the new realtime value visible to all wall-clock observers, including
  `clock_gettime`, `gettimeofday`, and filesystem timestamp providers.
- Keep the monotonic clock and every relative timeout independent of realtime
  steps.
- Re-evaluate absolute realtime waits and implement timerfd cancel-on-set
  behavior.
- Match Linux error ordering for an invalid clock ID, bad user pointer, invalid
  timestamp, missing privilege, and an unsupported clock range.
- Cover the behavior with a direct-syscall Starry test that failed with
  `Unimplemented syscall: clock_settime` before the implementation.

## Non-goals

- The obsolete `settimeofday(2)` syscall and timezone argument.
- Writing corrected time back to a hardware RTC.
- NTP discipline, frequency adjustment, leap-second handling, or time
  namespaces.
- Changing the platform's boot-time epoch discovery.

## Linux reference

The compatibility target is Linux v6.16 at commit
[`038d61fd642278bab63ee8ef722c50d10ab01e8f`](https://github.com/torvalds/linux/commit/038d61fd642278bab63ee8ef722c50d10ab01e8f).

Linux performs the relevant operations in this order:

1. [`clock_settime`](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/kernel/time/posix-timers.c#L1116-L1134)
   resolves the clock and rejects a non-settable clock before copying the user
   timespec.
2. [`do_sys_settimeofday64`](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/kernel/time/time.c#L169-L200)
   validates the timestamp before running the security hook.
3. [`cap_settime`](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/security/commoncap.c#L142-L146)
   requires `CAP_SYS_TIME`.
4. [`do_settimeofday64`](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/kernel/time/timekeeping.c#L1376-L1413)
   rejects realtime values before current monotonic time, publishes the new
   wall-to-monotonic offset, and notifies clock-change observers.
5. [`timerfd`](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/fs/timerfd.c#L90-L173)
   distinguishes monotonic and realtime timers; a cancel-on-set realtime timer
   reports `ECANCELED` from
   [`read`](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/fs/timerfd.c#L263-L307).
6. Periodic POSIX timers use
   [`hrtimer_forward`](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/kernel/time/posix-timers.c#L287-L327)
   to move directly past every elapsed interval, merge the missed expirations
   into one notification, and clamp the reported `si_overrun` to `INT_MAX`.

The public ABI and errno descriptions are also documented by
[`clock_settime(2)`](https://man7.org/linux/man-pages/man2/clock_settime.2.html)
and [`timerfd_create(2)`](https://man7.org/linux/man-pages/man2/timerfd_create.2.html).

## Design

### One shared realtime adjustment

`ax-plat` retains the platform epoch as an immutable boot reference and adds a
single signed nanosecond adjustment:

```text
monotonic counter + platform epoch + realtime adjustment = wall clock
```

`set_wall_time` validates the requested range and atomically publishes that
adjustment with release ordering. `wall_time` observes it with acquire ordering.
The monotonic counter is never modified. This location is intentionally below
StarryOS: `ax-hal`, filesystem timestamp providers, and Starry syscall readers
then observe one coherent system clock instead of maintaining independent
offsets.

The adjustment is an `i64` nanosecond delta. Together with Starry's Linux
`TIME_SETTOD_SEC_MAX` check, it supports the same practical ktime range while
keeping reads lock-free.

### Syscall boundary and error order

`sys_clock_settime` implements only `CLOCK_REALTIME`:

1. reject every other clock ID with `EINVAL`;
2. copy the user `timespec`, yielding `EFAULT` for an invalid pointer;
3. reject negative/out-of-range fields and unsupported values with `EINVAL`;
4. require `CAP_SYS_TIME`, yielding `EPERM`;
5. publish the shared adjustment and notify waiters.

The clock-ID check intentionally precedes pointer access. Timestamp validation
intentionally precedes the capability check. These choices are part of the
Linux-visible errno priority, not implementation convenience.

Starry's current credential model grants the initial root credential all
capabilities and clears them when the test switches to UID 65534. The capability
check is system-wide; time namespaces are outside this change.

### Clock-domain deadlines

A wall-clock step must affect absolute realtime deadlines but not relative or
monotonic waits. The implementation therefore makes clock domains explicit:

| Consumer | Relative deadline | Absolute realtime deadline |
| --- | --- | --- |
| `timerfd` | monotonic | realtime; optional cancel-on-set |
| POSIX timers | monotonic | realtime |
| `ITIMER_REAL` and `alarm` | monotonic | not applicable |
| futex waits | monotonic | realtime for `FUTEX_WAIT_BITSET` with `FUTEX_CLOCK_REALTIME` |
| POSIX message queues | not applicable | realtime |
| AIO, I/O multiplexing, socket, and USB timeouts | monotonic | not applicable |

`axtask::timeout_at_wall` listens for a clock-change generation event. It pins
the underlying future once, then rebuilds only its monotonic timer whenever the
published realtime clock changes. A generation check around listener
registration prevents a missed wakeup.

Starry's alarm dispatcher stores tagged monotonic/realtime deadlines. Because
values from different clock domains cannot be ordered by their raw timestamp,
the dispatcher selects the entry with the smallest current remaining duration
and sleeps against a monotonic deadline. A clock-change event wakes it to
re-evaluate realtime entries.

When a periodic POSIX realtime timer becomes overdue after a wall-clock step,
one expiration poll computes the full lag in units of the timer interval. It
moves the deadline to the first interval strictly after the adjusted clock,
queues one `SI_TIMER` notification, and records the additional expirations in
`si_overrun`, clamped to `INT_MAX`. This prevents the alarm task from repeatedly
re-registering a deadline that is still in the past. The existing missing
`timer_getoverrun(2)` syscall is outside this clock-setting change; the overrun
value is nevertheless visible through `siginfo_t` and `signalfd_siginfo`.

Timerfds are tracked through a weak-reference registry. Closing a timerfd
immediately unregisters its weak reference, so creating and closing timerfds
cannot retain dead `Arc` allocations until the next clock step. A realtime
step snapshots live timerfds while holding the registry lock, then releases
that lock before inspecting per-timer state. Absolute realtime timers registered
with `TFD_TIMER_CANCEL_ON_SET` are marked as canceled, poll/read waiters are
woken, and one read consumes `ECANCELED`. Relative and monotonic timerfds are
never marked for cancellation.

A timerfd expiration publishes one tick and parks the timer task. A normal
`read` or `timerfd_gettime` advances an expired periodic deadline past all
missed intervals and rearms it. This matches Linux's lazy periodic restart.
An `ECANCELED` read instead consumes the cancellation and pending ticks without
restarting an expired timer. Its next expiration then requires an explicit
`timerfd_settime`. Cancellation does not stop a future, unexpired deadline:
Linux's clock-change callback leaves that underlying timer armed, so reading
`ECANCELED` must preserve it.

### Concurrency and publication order

The setter publishes the atomic realtime adjustment before notifying timerfd,
alarm, and general wall-deadline observers. After any notification, a waiter
that acquires the adjustment sees the new clock value. The general event also
uses a generation counter so a change racing listener registration is observed
either as an event or as a generation mismatch.

Timerfd cancellation state and expiration counts are serialized by the timerfd
state mutex. The cancellation marker is published before poll waiters are
woken, and `timerfd_settime` clears both stale expirations and cancellation
state under the same mutex.

## Alternatives considered

### Starry-only offset

Rejected. `clock_gettime` could appear correct while filesystem timestamps and
other `ax-hal` consumers still used the boot epoch. That split clock would not
fix Cargo's timestamp decisions in the BuildStorm workflow.

### Mutating each platform's epoch implementation

Rejected. It would require a new mutable platform contract and duplicate
synchronization in static and dynamic platform implementations. A shared
adjustment above the platform epoch keeps platform RTC discovery unchanged.

### Converting every deadline to wall time

Rejected. A forward clock correction would immediately expire relative I/O and
scheduler timeouts; a backward correction would extend them. Tagged domains
preserve Linux's distinction between elapsed-time and calendar-time semantics.

## Validation

The deterministic regression is
`qemu/system/syscall-test-clock-settime`. It directly invokes
`SYS_clock_settime` and verifies:

- `EINVAL` wins over `EFAULT` for a non-settable clock;
- a bad realtime pointer returns `EFAULT`;
- an invalid nanosecond value returns `EINVAL`;
- realtime before the current monotonic clock and Linux's upper ktime boundary
  return `EINVAL`;
- an unprivileged child receives `EPERM`;
- root can move realtime forward while monotonic time does not jump;
- `gettimeofday` and a written-and-closed file observe the adjusted clock;
- a relative timerfd remains pending;
- an absolute realtime cancel-on-set timerfd returns `ECANCELED`;
- after consuming cancellation on an expired periodic timerfd, a second
  nonblocking read returns `EAGAIN` and `timerfd_gettime` reports no pending
  deadline; an explicit rearm produces a new expiration;
- consuming cancellation on a future timerfd preserves its original deadline;
- a periodic absolute realtime POSIX timer survives a 120-second forward step
  by queuing one signal, reporting the merged count through `si_overrun`, and
  publishing a next deadline in the future;
- the original clock is restored using monotonic elapsed time.

Before implementation, the AArch64 run failed deterministically with
`Unimplemented syscall: clock_settime`. The required command is:

```sh
cargo xtask starry test qemu --arch aarch64 \
  -c qemu/system/syscall-test-clock-settime
```

The physical-board acceptance flow booted Linux, prepared and deployed a native
`tg-xtask` plus the source tree, then booted StarryOS and corrected realtime
before invoking Cargo. The board emitted `BUILDSTORM_CLOCK ok` before the exact
`tg-xtask arceos build` command, confirming that the BuildStorm workload no
longer starts Cargo with a 1970 system clock. The later build currently exposes
a separate, reproducible AArch64 userspace alignment fault; that runtime issue
is not part of this syscall change.

## Syscall compatibility map

| Syscall | Conclusion | Standard/reference | Basis |
| --- | --- | --- | --- |
| `clock_settime` | compatible for `CLOCK_REALTIME` | [`clock_settime(2)`](https://man7.org/linux/man-pages/man2/clock_settime.2.html), [Linux implementation](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/kernel/time/posix-timers.c#L1116-L1134) | Matches supported clock, pointer/field/capability error order, range validation, publication, and notifications. |
| `clock_gettime` | no regression | [`clock_gettime(2)`](https://man7.org/linux/man-pages/man2/clock_gettime.2.html) | Realtime reads the shared adjustment; monotonic clocks remain unchanged. |
| `gettimeofday` | no regression | [`gettimeofday(2)`](https://man7.org/linux/man-pages/man2/gettimeofday.2.html) | Reads the same adjusted realtime value. |
| `time` | no regression | [`time(2)`](https://man7.org/linux/man-pages/man2/time.2.html) | The x86_64 entry reads the same adjusted realtime seconds. |
| `timerfd_settime` | compatible for clock-step behavior | [`timerfd_create(2)`](https://man7.org/linux/man-pages/man2/timerfd_create.2.html), [Linux timerfd clock-change path](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/fs/timerfd.c#L90-L173) | Relative timers stay monotonic; absolute realtime timers are re-evaluated; cancel-on-set is armed only for that domain. |
| `timerfd_gettime` | compatible for adjusted deadlines | [`timerfd_create(2)`](https://man7.org/linux/man-pages/man2/timerfd_create.2.html) | Remaining time is computed in the deadline's explicit clock domain. |
| `read` (timerfd) | compatible for cancel-on-set | [Linux timerfd read path](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/fs/timerfd.c#L263-L307) | One read consumes cancellation and ticks, returns `ECANCELED`, and does not restart an expired timer. A future, unexpired deadline remains armed. |
| `timer_settime` | improved clock-step compatibility | [`timer_settime(2)`](https://man7.org/linux/man-pages/man2/timer_settime.2.html), [Linux periodic rearm](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/kernel/time/posix-timers.c#L287-L327) | Relative timers use monotonic deadlines; absolute `CLOCK_REALTIME` timers retain realtime deadlines and are re-evaluated. Missed periodic expirations are merged into one notification, the next deadline skips all elapsed intervals, and `si_overrun` is clamped to `INT_MAX`. |
| `timer_gettime` | no regression | [`timer_gettime(2)`](https://man7.org/linux/man-pages/man2/timer_gettime.2.html) | Remaining time is derived from the timer's tagged domain. |
| `setitimer` | no regression | [`getitimer(2)`](https://man7.org/linux/man-pages/man2/getitimer.2.html) | `ITIMER_REAL` remains an elapsed-time interval across realtime changes. |
| `getitimer` | no regression | [`getitimer(2)`](https://man7.org/linux/man-pages/man2/getitimer.2.html) | Reports monotonic remaining duration. |
| `alarm` | no regression | [`alarm(2)`](https://man7.org/linux/man-pages/man2/alarm.2.html) | Uses the same monotonic process-real-timer state. |
| `futex` | improved realtime absolute-wait behavior | [`futex(2)`](https://man7.org/linux/man-pages/man2/futex.2.html), [Linux operation validation](https://github.com/torvalds/linux/blob/038d61fd642278bab63ee8ef722c50d10ab01e8f/kernel/futex/syscalls.c#L88-L97) | `FUTEX_WAIT_BITSET` with `FUTEX_CLOCK_REALTIME` rebuilds its absolute realtime wait after a clock step; unsupported clock-flag combinations return `ENOSYS`. |
| `mq_timedsend` | no regression | [`mq_timedsend(3)`](https://man7.org/linux/man-pages/man3/mq_timedsend.3.html) | Existing wall-deadline wait now receives realtime-change notifications. |
| `mq_timedreceive` | no regression | [`mq_timedreceive(3)`](https://man7.org/linux/man-pages/man3/mq_timedreceive.3.html) | Existing wall-deadline wait now receives realtime-change notifications. |
| `io_getevents` | corrected relative-time behavior | [`io_getevents(2)`](https://man7.org/linux/man-pages/man2/io_getevents.2.html) | Its relative timeout now uses monotonic time. |
| `recvmmsg` | corrected relative-time behavior | [`recvmmsg(2)`](https://man7.org/linux/man-pages/man2/recvmmsg.2.html) | Its relative timeout accounting now uses monotonic time. |
| `select`, `poll`, and `epoll_wait` (ArceOS POSIX API) | corrected relative-time behavior | [`select(2)`](https://man7.org/linux/man-pages/man2/select.2.html), [`poll(2)`](https://man7.org/linux/man-pages/man2/poll.2.html), [`epoll_wait(2)`](https://man7.org/linux/man-pages/man2/epoll_wait.2.html) | Their relative timeout accounting now uses monotonic time, so the shared realtime adjustment cannot shorten or extend a wait. |

## Rollback

The change has no persistent clock-state format. Rolling back removes the
in-memory adjustment and notifier paths; the platform epoch and on-disk
filesystem format are untouched. Rebooting also discards the adjustment.
