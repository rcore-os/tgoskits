//! Advisory file locking: POSIX (`fcntl` `F_SETLK`/`F_GETLK`), Open File
//! Description (`fcntl` `F_OFD_*`), and BSD `flock(2)`.
//!
//! POSIX and OFD locks share a single conflict space (Linux `fs/locks.c`
//! treats both as `FL_POSIX` for conflict purposes); only the *owner*
//! identity differs (process vs open file description). `flock(2)` is an
//! independent conflict space.
//!
//! Limitations versus Linux (intentional, see fcntl bug-cases for what is
//! covered):
//!   * Mandatory (kernel-enforced) locking is not supported.
//!   * `EDEADLK` deadlock detection on `F_SETLKW` is not implemented;
//!     a circular wait will block forever (or until a signal arrives,
//!     which delivers `EINTR` as POSIX requires).
//!
//! POSIX release semantics (matching Linux `fs/locks.c`):
//!   * Process exit drops every POSIX lock the exiting pid still owns,
//!     across all inodes — see [`release_pid_locks`].
//!   * Closing **any** fd that refers to an inode drops every POSIX lock
//!     the calling pid owns on that inode (the well-known POSIX
//!     "close-eats-locks" rule). Wired through `close(2)`, `close_range(2)`
//!     and `execve(2)` CLOEXEC — see [`release_inode_posix_locks`].
//!   * OFD locks are owned by the open file description, so they get
//!     released automatically when the last reference to that OFD goes
//!     away (their entries are pruned lazily via `Weak::strong_count`).

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::ffi::c_int;

use ax_errno::{AxError, AxResult};
use ax_task::current;
use linux_raw_sys::general::{
    F_GETLK, F_OFD_GETLK, F_OFD_SETLK, F_OFD_SETLKW, F_RDLCK, F_SETLK, F_SETLKW, F_UNLCK, F_WRLCK,
    LOCK_EX, LOCK_NB, LOCK_SH, LOCK_UN, O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY, SEEK_CUR, SEEK_END,
    SEEK_SET, flock64,
};
use spin::RwLock;
use starry_process::Pid;

use crate::{
    file::{File, FileLike, get_file_like},
    mm::UserPtr,
    task::{AsThread, futex::WaitQueue},
};

type InodeKey = (u64, u64); // (device, inode_no)
type OfdAddr = usize;

/// Linux convention: `F_OFD_GETLK` reports `l_pid = -1` for an OFD owner.
const OFD_PID_REPORTED: i32 = -1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LockKind {
    Read,
    Write,
}

/// Owner of an entry in the fcntl POSIX/OFD lock table.
#[derive(Debug)]
enum FOwner {
    Posix {
        pid: Pid,
    },
    Ofd {
        addr: OfdAddr,
        weak: Weak<dyn FileLike>,
    },
}

impl FOwner {
    /// Owner identity for the purposes of "do these two entries belong to
    /// the same lock holder, so they merge / don't conflict".
    fn same_as(&self, other: &FOwner) -> bool {
        match (self, other) {
            (FOwner::Posix { pid: a }, FOwner::Posix { pid: b }) => a == b,
            (FOwner::Ofd { addr: a, .. }, FOwner::Ofd { addr: b, .. }) => a == b,
            _ => false,
        }
    }

    /// pid value to report back via `F_GETLK`.
    fn report_pid(&self) -> i32 {
        match self {
            FOwner::Posix { pid } => *pid as i32,
            FOwner::Ofd { .. } => OFD_PID_REPORTED,
        }
    }

    /// True if the entry's owner is no longer alive (OFD whose backing
    /// file has been fully closed). POSIX entries never expire here.
    fn is_dead(&self) -> bool {
        match self {
            FOwner::Posix { .. } => false,
            FOwner::Ofd { weak, .. } => weak.strong_count() == 0,
        }
    }
}

#[derive(Debug)]
struct FLockEntry {
    /// Half-open range `[start, end)`. `end == i64::MAX` represents
    /// `l_len == 0` ("lock to end of file").
    start: i64,
    end: i64,
    kind: LockKind,
    owner: FOwner,
}

/// fcntl POSIX + OFD locks share one space.
static FCNTL_LOCKS: RwLock<BTreeMap<InodeKey, Vec<FLockEntry>>> = RwLock::new(BTreeMap::new());

/// Per-inode waiters parked by `F_SETLKW`/`F_OFD_SETLKW` until a
/// conflicting lock is released. Wakers are called from every code path
/// that may shrink an inode's `FCNTL_LOCKS` entries (explicit `F_UNLCK`,
/// process exit, close-eats-locks, OFD release on last close).
static LOCK_WAITERS: RwLock<BTreeMap<InodeKey, Arc<WaitQueue>>> = RwLock::new(BTreeMap::new());

#[derive(Debug)]
struct FlockEntry {
    addr: OfdAddr,
    weak: Weak<dyn FileLike>,
    kind: LockKind,
}

/// flock(2) entries: at most one entry per (inode, OFD).
static FLOCK_LOCKS: RwLock<BTreeMap<InodeKey, Vec<FlockEntry>>> = RwLock::new(BTreeMap::new());

/// Per-inode waiters parked by blocking `flock(LOCK_SH/LOCK_EX)` (without
/// `LOCK_NB`) until a conflicting OFD-level entry is released. Independent
/// of [`LOCK_WAITERS`] because fcntl record locks and flock(2) live in
/// separate conflict spaces (Linux `fs/locks.c`: `FL_POSIX` vs `FL_FLOCK`).
/// Wakers fire from every path that shrinks [`FLOCK_LOCKS`] — explicit
/// `LOCK_UN`, downgrades, and OFD release on last close.
static FLOCK_WAITERS: RwLock<BTreeMap<InodeKey, Arc<WaitQueue>>> = RwLock::new(BTreeMap::new());

// ─── helpers ───────────────────────────────────────────────────────────

fn ofd_addr(arc: &Arc<dyn FileLike>) -> OfdAddr {
    Arc::as_ptr(arc) as *const () as usize
}

fn current_pid() -> Pid {
    current().as_thread().proc_data.proc.pid()
}

/// Resolve `fd` to an inode-keyed lockable file. Returns `EBADF` for fds
/// that have no inode (pipes, sockets, epoll, ...), matching Linux's
/// behavior of rejecting flock/fcntl-locks on non-files.
fn lockable(fd: c_int) -> AxResult<(InodeKey, Arc<dyn FileLike>)> {
    let f = get_file_like(fd)?;
    let key = f.inode_key().ok_or(AxError::BadFileDescriptor)?;
    Ok((key, f))
}

/// Resolve `flock64.l_start` relative to `l_whence`, matching Linux
/// `flock_to_posix_lock()`:
///   * `SEEK_SET` — absolute offset, returned unchanged.
///   * `SEEK_CUR` — relative to the fd's current read/write cursor.
///   * `SEEK_END` — relative to the file's current size.
///
/// `SEEK_CUR` / `SEEK_END` are only meaningful for regular files; on a
/// directory fd (no cursor / size in the byte-offset sense) they return
/// `EINVAL`. Overflow returns `EINVAL`.
fn resolve_l_start(file: &Arc<dyn FileLike>, l_whence: i16, l_start: i64) -> AxResult<i64> {
    let whence = l_whence as u32;
    if whence == SEEK_SET {
        return Ok(l_start);
    }
    if whence != SEEK_CUR && whence != SEEK_END {
        return Err(AxError::InvalidInput);
    }
    let regular = file.downcast_ref::<File>().ok_or(AxError::InvalidInput)?;
    let base = if whence == SEEK_CUR {
        regular.inner().position().ok_or(AxError::InvalidInput)?
    } else {
        regular
            .inner()
            .location()
            .len()
            .map_err(|_| AxError::InvalidInput)?
    };
    // Linux uses i_size / cursor as i64-relative arithmetic; reject anything
    // that does not fit in i64.
    let base_i64 = i64::try_from(base).map_err(|_| AxError::InvalidInput)?;
    base_i64.checked_add(l_start).ok_or(AxError::InvalidInput)
}

/// Translate a half-open `[l_start, l_start + l_len)` description (where
/// `l_start` is *already* the absolute offset resolved by
/// [`resolve_l_start`]) into a half-open `[start, end)` range, matching
/// Linux `flock_to_posix_lock()`:
///   * `l_len > 0` — `[l_start, l_start + l_len)`.
///   * `l_len == 0` — `[l_start, i64::MAX)` (to end of file).
///   * `l_len < 0` — `[l_start + l_len, l_start)` (reverse range; the
///     resolved start must be non-negative).
///
/// Any overflow or a resolved start < 0 returns `EINVAL`.
fn flock_range(l_start: i64, l_len: i64) -> AxResult<(i64, i64)> {
    if l_len == 0 {
        if l_start < 0 {
            return Err(AxError::InvalidInput);
        }
        return Ok((l_start, i64::MAX));
    }
    let (start, end) = if l_len > 0 {
        let end = l_start.checked_add(l_len).ok_or(AxError::InvalidInput)?;
        (l_start, end)
    } else {
        let start = l_start.checked_add(l_len).ok_or(AxError::InvalidInput)?;
        (start, l_start)
    };
    if start < 0 {
        return Err(AxError::InvalidInput);
    }
    Ok((start, end))
}

fn ranges_overlap(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> bool {
    a_start < b_end && b_start < a_end
}

/// Read–read is the only mutually compatible pair.
fn kinds_conflict(a: LockKind, b: LockKind) -> bool {
    !(a == LockKind::Read && b == LockKind::Read)
}

/// Linux requires the fd to be open for the matching access mode before a
/// POSIX/OFD record lock of that kind may be installed: F_RDLCK needs the
/// fd to be readable, F_WRLCK needs it to be writable. Mismatch → EBADF.
fn fd_supports_kind(file: &Arc<dyn FileLike>, kind: LockKind) -> bool {
    let acc = file.open_flags() & O_ACCMODE;
    match kind {
        LockKind::Read => acc == O_RDONLY || acc == O_RDWR,
        LockKind::Write => acc == O_WRONLY || acc == O_RDWR,
    }
}

// ─── fcntl: clear same-owner overlap so we can install a new lock ─────

/// Remove (and split where needed) any same-owner entries on `inode` that
/// overlap `[start, end)`. Used by both F_UNLCK and SETLK insert paths.
/// Returns `true` if at least one entry was touched — including the case
/// where an entry was merely shrunk by a tail/head split (the overall
/// `entries.len()` is unchanged but a previously-locked sub-range is now
/// free, so anyone parked on it must be woken).
fn clear_owner_overlap(
    entries: &mut Vec<FLockEntry>,
    owner: &FOwner,
    start: i64,
    end: i64,
) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < entries.len() {
        let e = &entries[i];
        if !e.owner.same_as(owner) || !ranges_overlap(e.start, e.end, start, end) {
            i += 1;
            continue;
        }
        changed = true;
        let (es, ee, ek) = (e.start, e.end, e.kind);
        // Snapshot owner via the per-arm clone — Posix is trivially Copy
        // semantics, OFD must clone its Weak.
        let snap_owner = match &e.owner {
            FOwner::Posix { pid } => FOwner::Posix { pid: *pid },
            FOwner::Ofd { addr, weak } => FOwner::Ofd {
                addr: *addr,
                weak: weak.clone(),
            },
        };
        entries.swap_remove(i);
        // Re-insert the head fragment [es, start) if any.
        if es < start {
            entries.push(FLockEntry {
                start: es,
                end: start,
                kind: ek,
                owner: match &snap_owner {
                    FOwner::Posix { pid } => FOwner::Posix { pid: *pid },
                    FOwner::Ofd { addr, weak } => FOwner::Ofd {
                        addr: *addr,
                        weak: weak.clone(),
                    },
                },
            });
        }
        // Re-insert the tail fragment [end, ee) if any.
        if ee > end {
            entries.push(FLockEntry {
                start: end,
                end: ee,
                kind: ek,
                owner: snap_owner,
            });
        }
        // Don't advance i — swap_remove brought a fresh entry into i.
    }
    changed
}

/// Walk `entries` and find the first record that conflicts with a request
/// of `kind` on `[start, end)` from `requester`. Dead OFD entries are
/// pruned in passing.
fn find_conflict<'a>(
    entries: &'a mut Vec<FLockEntry>,
    requester: &FOwner,
    start: i64,
    end: i64,
    kind: LockKind,
) -> Option<&'a FLockEntry> {
    entries.retain(|e| !e.owner.is_dead());
    entries.iter().find(|e| {
        !e.owner.same_as(requester)
            && ranges_overlap(e.start, e.end, start, end)
            && kinds_conflict(e.kind, kind)
    })
}

// ─── per-inode wait queue (F_SETLKW backbone) ──────────────────────────

/// Get-or-create the wait queue for a single inode. We never garbage
/// collect entries from `LOCK_WAITERS`: a wait queue carries no per-task
/// state once it is empty, and waiters always re-check conflict after
/// being woken, so a stale (but empty) queue costs nothing.
fn lock_waiters(key: InodeKey) -> Arc<WaitQueue> {
    if let Some(wq) = LOCK_WAITERS.read().get(&key) {
        return wq.clone();
    }
    LOCK_WAITERS
        .write()
        .entry(key)
        .or_insert_with(|| Arc::new(WaitQueue::new()))
        .clone()
}

/// Wake every task parked on `key`. MUST be called without `FCNTL_LOCKS`
/// held to keep the lock order `WaitQueue → FCNTL_LOCKS`: waiters take
/// the wait-queue mutex first and the table lock second from inside
/// `wait_if`'s condition closure, so a waker that already held the
/// table lock would invert that order and deadlock.
pub fn wake_lock_waiters(key: InodeKey) {
    let wq = LOCK_WAITERS.read().get(&key).cloned();
    if let Some(wq) = wq {
        wq.wake(usize::MAX, !0);
    }
}

/// Same as [`lock_waiters`] but for blocking `flock(2)`.
fn flock_waiters(key: InodeKey) -> Arc<WaitQueue> {
    if let Some(wq) = FLOCK_WAITERS.read().get(&key) {
        return wq.clone();
    }
    FLOCK_WAITERS
        .write()
        .entry(key)
        .or_insert_with(|| Arc::new(WaitQueue::new()))
        .clone()
}

/// Wake every task parked on `key` waiting for a flock(2) lock. Same
/// lock-order rules as [`wake_lock_waiters`].
pub fn wake_flock_waiters(key: InodeKey) {
    let wq = FLOCK_WAITERS.read().get(&key).cloned();
    if let Some(wq) = wq {
        wq.wake(usize::MAX, !0);
    }
}

// ─── fcntl entry points ────────────────────────────────────────────────

/// Build a fresh `FOwner` for the calling thread. Called per attempt
/// inside the F_SETLKW retry loop because OFD owners snapshot the
/// `Weak<dyn FileLike>` and Posix owners need the *current* pid (which
/// won't change for a single thread, but cloning is trivial).
fn make_owner(ofd: bool, file: &Arc<dyn FileLike>) -> FOwner {
    if ofd {
        FOwner::Ofd {
            addr: ofd_addr(file),
            weak: Arc::downgrade(file),
        }
    } else {
        FOwner::Posix { pid: current_pid() }
    }
}

/// Result of one attempt to install / clear a record lock. Carried out
/// of the FCNTL_LOCKS critical section so any wakeups happen with the
/// table lock released.
enum SetlkAttempt {
    Done { woke_others: bool },
    Conflict,
}

fn try_setlk_once(
    key: InodeKey,
    owner: FOwner,
    start: i64,
    end: i64,
    kind: Option<LockKind>,
) -> SetlkAttempt {
    let mut table = FCNTL_LOCKS.write();
    let entries = table.entry(key).or_default();
    entries.retain(|e| !e.owner.is_dead());

    let attempt = match kind {
        None => {
            let woke_others = clear_owner_overlap(entries, &owner, start, end);
            SetlkAttempt::Done { woke_others }
        }
        Some(k) => {
            if find_conflict(entries, &owner, start, end, k).is_some() {
                SetlkAttempt::Conflict
            } else {
                let woke_others = clear_owner_overlap(entries, &owner, start, end);
                entries.push(FLockEntry {
                    start,
                    end,
                    kind: k,
                    owner,
                });
                SetlkAttempt::Done { woke_others }
            }
        }
    };
    if entries.is_empty() {
        table.remove(&key);
    }
    attempt
}

/// Common impl for `F_SETLK` / `F_SETLKW` (POSIX) and `F_OFD_SETLK` /
/// `F_OFD_SETLKW` (OFD). When `wait` is true, the caller blocks on the
/// per-inode wait queue until the conflict clears or a signal arrives
/// (returning `EINTR` per POSIX). When `wait` is false, conflicts return
/// `EAGAIN` immediately.
pub fn fcntl_setlk(fd: c_int, arg: usize, ofd: bool, wait: bool) -> AxResult<isize> {
    let fl = UserPtr::<flock64>::from(arg).get_as_mut()?;
    // POSIX.1-2024 / Linux: F_OFD_SETLK{,W} require l_pid to be 0.
    if ofd && fl.l_pid != 0 {
        return Err(AxError::InvalidInput);
    }
    let (key, file) = lockable(fd)?;
    let abs_start = resolve_l_start(&file, fl.l_whence, fl.l_start)?;
    let (start, end) = flock_range(abs_start, fl.l_len)?;

    let kind = match fl.l_type as u32 {
        F_UNLCK => None,
        F_RDLCK => Some(LockKind::Read),
        F_WRLCK => Some(LockKind::Write),
        _ => return Err(AxError::InvalidInput),
    };

    // Linux: installing a record lock requires the fd to be open for the
    // matching access mode. F_UNLCK is exempt — you can always release.
    if let Some(k) = kind
        && !fd_supports_kind(&file, k)
    {
        return Err(AxError::BadFileDescriptor);
    }

    loop {
        let owner = make_owner(ofd, &file);
        match try_setlk_once(key, owner, start, end, kind) {
            SetlkAttempt::Done { woke_others } => {
                if woke_others {
                    wake_lock_waiters(key);
                }
                return Ok(0);
            }
            SetlkAttempt::Conflict => {
                if !wait {
                    return Err(AxError::WouldBlock);
                }
                // Park on the inode's wait queue. The condition re-checks
                // conflict while holding only the wq mutex (which itself
                // takes FCNTL_LOCKS internally) so there is no chance of
                // missing a wakeup that lands between our outer attempt
                // and the sleep.
                let wq = lock_waiters(key);
                wq.wait_if(!0u32, None, || {
                    let mut table = FCNTL_LOCKS.write();
                    let Some(entries) = table.get_mut(&key) else {
                        return false;
                    };
                    entries.retain(|e| !e.owner.is_dead());
                    let owner = make_owner(ofd, &file);
                    let still_blocked =
                        find_conflict(entries, &owner, start, end, kind.unwrap()).is_some();
                    if entries.is_empty() {
                        table.remove(&key);
                    }
                    still_blocked
                })?;
                // Loop and retry.
            }
        }
    }
}

/// Common impl for `F_GETLK` (POSIX) and `F_OFD_GETLK` (OFD). Reports the
/// first conflicting lock, or sets `l_type = F_UNLCK` if the requested
/// range is free.
pub fn fcntl_getlk(fd: c_int, arg: usize, ofd: bool) -> AxResult<isize> {
    let fl = UserPtr::<flock64>::from(arg).get_as_mut()?;
    // POSIX.1-2024 / Linux: F_OFD_GETLK requires l_pid to be 0.
    if ofd && fl.l_pid != 0 {
        return Err(AxError::InvalidInput);
    }
    let req_kind = match fl.l_type as u32 {
        F_RDLCK => LockKind::Read,
        F_WRLCK => LockKind::Write,
        _ => return Err(AxError::InvalidInput),
    };
    let (key, file) = lockable(fd)?;
    let abs_start = resolve_l_start(&file, fl.l_whence, fl.l_start)?;
    let (start, end) = flock_range(abs_start, fl.l_len)?;

    let requester = if ofd {
        FOwner::Ofd {
            addr: ofd_addr(&file),
            weak: Arc::downgrade(&file),
        }
    } else {
        FOwner::Posix { pid: current_pid() }
    };

    let mut table = FCNTL_LOCKS.write();
    let (report, empty_after) = {
        let entries = table.entry(key).or_default();
        let report = find_conflict(entries, &requester, start, end, req_kind).map(|e| {
            (
                e.kind,
                e.owner.report_pid(),
                e.start,
                if e.end == i64::MAX {
                    0
                } else {
                    e.end - e.start
                },
            )
        });
        (report, entries.is_empty())
    };
    if empty_after {
        table.remove(&key);
    }

    if let Some((kind, pid, l_start, l_len)) = report {
        fl.l_type = (if kind == LockKind::Read {
            F_RDLCK
        } else {
            F_WRLCK
        }) as i16;
        fl.l_whence = SEEK_SET as i16;
        fl.l_start = l_start;
        fl.l_len = l_len;
        fl.l_pid = pid;
    } else {
        fl.l_type = F_UNLCK as i16;
    }
    Ok(0)
}

/// Top-level dispatch from `sys_fcntl`. Returns `Some(result)` if `cmd`
/// is one of the lock commands; otherwise `None` so the caller can fall
/// through to other fcntl handling.
pub fn dispatch_fcntl(fd: c_int, cmd: c_int, arg: usize) -> Option<AxResult<isize>> {
    let cmd = cmd as u32;
    Some(match cmd {
        F_SETLK => fcntl_setlk(fd, arg, false, false),
        F_SETLKW => fcntl_setlk(fd, arg, false, true),
        F_OFD_SETLK => fcntl_setlk(fd, arg, true, false),
        F_OFD_SETLKW => fcntl_setlk(fd, arg, true, true),
        F_GETLK => fcntl_getlk(fd, arg, false),
        F_OFD_GETLK => fcntl_getlk(fd, arg, true),
        _ => return None,
    })
}

/// Release every POSIX (`fcntl`) lock owned by `pid`. Called from the
/// process-exit hook (`task::ops`) so that a process that crashes or
/// exits without explicit `F_UNLCK` does not leave its records pinned in
/// `FCNTL_LOCKS`. OFD entries are untouched: their owner is the open
/// file description, which is already cleaned up by `close_all_fds`
/// dropping the underlying `Arc<dyn FileLike>`.
pub fn release_pid_locks(pid: Pid) {
    let mut affected: Vec<InodeKey> = Vec::new();
    {
        let mut table = FCNTL_LOCKS.write();
        table.retain(|inode, entries| {
            let before = entries.len();
            entries.retain(|e| match &e.owner {
                FOwner::Posix { pid: p } => *p != pid,
                FOwner::Ofd { .. } => true,
            });
            if entries.len() != before {
                affected.push(*inode);
            }
            !entries.is_empty()
        });
    }
    // Wake outside the table-lock critical section to keep lock order.
    for key in affected {
        wake_lock_waiters(key);
    }
}

/// POSIX "close-eats-locks": closing **any** fd referring to an inode
/// drops every POSIX record lock the calling pid still holds on that
/// inode, even if the lock was acquired through a different fd. Linux
/// implements this in `fs/locks.c` via `locks_remove_posix()` driven by
/// `filp_close()`; we wire the equivalent here from `close_file_like`,
/// `sys_close_range` and the `execve` CLOEXEC sweep.
///
/// OFD entries are owned by the open file description, not the pid, so
/// they are deliberately left in place — they age out via
/// `Weak::strong_count` once the underlying `Arc<dyn FileLike>` is gone.
pub fn release_inode_posix_locks(pid: Pid, key: (u64, u64)) {
    let woke_someone = {
        let mut table = FCNTL_LOCKS.write();
        let Some(entries) = table.get_mut(&key) else {
            return;
        };
        let before = entries.len();
        entries.retain(|e| match &e.owner {
            FOwner::Posix { pid: p } => *p != pid,
            FOwner::Ofd { .. } => true,
        });
        let changed = entries.len() != before;
        if entries.is_empty() {
            table.remove(&key);
        }
        changed
    };
    if woke_someone {
        wake_lock_waiters(key);
    }
}

// ─── flock(2) ──────────────────────────────────────────────────────────

/// Outcome of one [`try_flock_once`] attempt.
enum FlockAttempt {
    Done,
    Conflict,
}

/// Try to install / clear a flock entry. Returns the outcome plus a
/// `mutated` flag set whenever the table was actually shrunk or
/// downgraded — including the conflict path, because Linux's non-atomic
/// conversion drops the caller's prior entry before checking peers, which
/// on its own may unblock a peer parked waiting for that entry to go away.
fn try_flock_once(
    key: InodeKey,
    addr: OfdAddr,
    file: &Arc<dyn FileLike>,
    kind: Option<LockKind>,
) -> (FlockAttempt, bool) {
    let mut table = FLOCK_LOCKS.write();
    let entries = table.entry(key).or_default();
    let before = entries.len();
    entries.retain(|e| e.weak.strong_count() != 0);

    let outcome = match kind {
        None => {
            // LOCK_UN: drop any entry held by this OFD.
            entries.retain(|e| e.addr != addr);
            FlockAttempt::Done
        }
        Some(want) => {
            // Linux flock(2) conversion is non-atomic: drop our own
            // existing entry first, THEN check conflicts. A failed
            // conversion therefore leaves the file unlocked, not rolled
            // back to the prior lock — matching `flock_lock_inode()` in
            // fs/locks.c and `man 2 flock` (NOTES, "Converting a lock").
            entries.retain(|e| e.addr != addr);
            let blocked = entries.iter().any(|e| kinds_conflict(e.kind, want));
            if blocked {
                FlockAttempt::Conflict
            } else {
                entries.push(FlockEntry {
                    addr,
                    weak: Arc::downgrade(file),
                    kind: want,
                });
                FlockAttempt::Done
            }
        }
    };
    let mutated = entries.len() != before;
    if entries.is_empty() {
        table.remove(&key);
    }
    (outcome, mutated)
}

/// Implementation of `sys_flock`. Supports `LOCK_SH`, `LOCK_EX`, `LOCK_UN`,
/// optionally OR'd with `LOCK_NB`. Without `LOCK_NB`, the caller is parked
/// on the per-inode flock wait queue until the conflict clears or a signal
/// arrives (returning `EINTR`). With `LOCK_NB`, conflicts return
/// `EWOULDBLOCK` immediately.
pub fn flock_op(fd: c_int, operation: c_int) -> AxResult<isize> {
    let op = operation as u32;
    let nonblock = op & LOCK_NB != 0;
    let kind = match op & !LOCK_NB {
        LOCK_SH => Some(LockKind::Read),
        LOCK_EX => Some(LockKind::Write),
        LOCK_UN => None,
        _ => return Err(AxError::InvalidInput),
    };

    let (key, file) = lockable(fd)?;
    let addr = ofd_addr(&file);

    loop {
        let (outcome, mutated) = try_flock_once(key, addr, &file, kind);
        if mutated {
            wake_flock_waiters(key);
        }
        match outcome {
            FlockAttempt::Done => return Ok(0),
            FlockAttempt::Conflict => {
                if nonblock {
                    return Err(AxError::WouldBlock);
                }
                // Park on the inode's flock wait queue. Condition re-checks
                // conflict from inside the wq mutex (which itself takes
                // FLOCK_LOCKS), so any wake landing between our outer
                // attempt and the sleep is not lost.
                let want = kind.unwrap();
                let wq = flock_waiters(key);
                wq.wait_if(!0u32, None, || {
                    let table = FLOCK_LOCKS.read();
                    let Some(entries) = table.get(&key) else {
                        return false;
                    };
                    entries.iter().any(|e| {
                        e.weak.strong_count() != 0 && e.addr != addr && kinds_conflict(e.kind, want)
                    })
                })?;
                // Loop and retry.
            }
        }
    }
}
