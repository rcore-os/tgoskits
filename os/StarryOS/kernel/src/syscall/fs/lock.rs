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
//!   * `F_SETLKW` and blocking `flock` do not actually block — they return
//!     `EAGAIN` on conflict, just like the non-blocking variants.
//!   * POSIX `close()`-triggered release of all per-pid locks on an inode
//!     is not implemented; locks live until explicit `F_UNLCK` or process
//!     exit.
//!   * Only `SEEK_SET` is accepted for `l_whence`.
//!   * Mandatory (kernel-enforced) locking is not supported.

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::ffi::c_int;

use ax_errno::{AxError, AxResult};
use ax_task::current;
use linux_raw_sys::general::{
    F_OFD_GETLK, F_OFD_SETLK, F_OFD_SETLKW, F_RDLCK, F_SETLK, F_SETLKW, F_UNLCK, F_WRLCK, LOCK_EX,
    LOCK_NB, LOCK_SH, LOCK_UN, SEEK_SET, flock64,
};
use spin::RwLock;
use starry_process::Pid;

use crate::{
    file::{FileLike, get_file_like},
    mm::UserPtr,
    task::AsThread,
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
    Posix { pid: Pid },
    Ofd { addr: OfdAddr, weak: Weak<dyn FileLike> },
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

#[derive(Debug)]
struct FlockEntry {
    addr: OfdAddr,
    weak: Weak<dyn FileLike>,
    kind: LockKind,
}

/// flock(2) entries: at most one entry per (inode, OFD).
static FLOCK_LOCKS: RwLock<BTreeMap<InodeKey, Vec<FlockEntry>>> = RwLock::new(BTreeMap::new());

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

/// Translate the `flock64.l_start` / `l_len` pair into a half-open
/// `[start, end)` range. `l_len == 0` means "to end of file"; we model
/// that as `i64::MAX`. Negative `l_len` (lock backwards from `l_start`)
/// is rejected for now — Linux supports it but no test in this tree
/// requires it.
fn flock_range(l_start: i64, l_len: i64) -> AxResult<(i64, i64)> {
    if l_start < 0 {
        return Err(AxError::InvalidInput);
    }
    if l_len < 0 {
        return Err(AxError::InvalidInput);
    }
    if l_len == 0 {
        Ok((l_start, i64::MAX))
    } else {
        let end = l_start.checked_add(l_len).ok_or(AxError::InvalidInput)?;
        Ok((l_start, end))
    }
}

fn ranges_overlap(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> bool {
    a_start < b_end && b_start < a_end
}

/// Read–read is the only mutually compatible pair.
fn kinds_conflict(a: LockKind, b: LockKind) -> bool {
    !(a == LockKind::Read && b == LockKind::Read)
}

// ─── fcntl: clear same-owner overlap so we can install a new lock ─────

/// Remove (and split where needed) any same-owner entries on `inode` that
/// overlap `[start, end)`. Used by both F_UNLCK and SETLK insert paths.
fn clear_owner_overlap(
    entries: &mut Vec<FLockEntry>,
    owner: &FOwner,
    start: i64,
    end: i64,
) {
    let mut i = 0;
    while i < entries.len() {
        let e = &entries[i];
        if !e.owner.same_as(owner) || !ranges_overlap(e.start, e.end, start, end) {
            i += 1;
            continue;
        }
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
    entries
        .iter()
        .find(|e| {
            !e.owner.same_as(requester)
                && ranges_overlap(e.start, e.end, start, end)
                && kinds_conflict(e.kind, kind)
        })
}

// ─── fcntl entry points ────────────────────────────────────────────────

/// Common impl for `F_SETLK` / `F_SETLKW` (POSIX) and `F_OFD_SETLK` /
/// `F_OFD_SETLKW` (OFD). Blocking is *not* implemented; `_wait` is
/// accepted for ABI completeness but never blocks.
pub fn fcntl_setlk(fd: c_int, arg: usize, ofd: bool, _wait: bool) -> AxResult<isize> {
    let fl = UserPtr::<flock64>::from(arg).get_as_mut()?;
    if fl.l_whence as u32 != SEEK_SET {
        return Err(AxError::InvalidInput);
    }
    let (start, end) = flock_range(fl.l_start, fl.l_len)?;
    let (key, file) = lockable(fd)?;

    let owner = if ofd {
        FOwner::Ofd {
            addr: ofd_addr(&file),
            weak: Arc::downgrade(&file),
        }
    } else {
        FOwner::Posix { pid: current_pid() }
    };

    let kind = match fl.l_type as u32 {
        F_UNLCK => None,
        F_RDLCK => Some(LockKind::Read),
        F_WRLCK => Some(LockKind::Write),
        _ => return Err(AxError::InvalidInput),
    };

    let mut table = FCNTL_LOCKS.write();
    let (conflict, empty_after) = {
        let entries = table.entry(key).or_default();
        entries.retain(|e| !e.owner.is_dead());

        let conflict = match kind {
            None => {
                clear_owner_overlap(entries, &owner, start, end);
                false
            }
            Some(k) => {
                if find_conflict(entries, &owner, start, end, k).is_some() {
                    true
                } else {
                    clear_owner_overlap(entries, &owner, start, end);
                    entries.push(FLockEntry {
                        start,
                        end,
                        kind: k,
                        owner,
                    });
                    false
                }
            }
        };
        (conflict, entries.is_empty())
    };
    if empty_after {
        table.remove(&key);
    }
    if conflict {
        return Err(AxError::WouldBlock);
    }
    Ok(0)
}

/// Common impl for `F_GETLK` (POSIX) and `F_OFD_GETLK` (OFD). Reports the
/// first conflicting lock, or sets `l_type = F_UNLCK` if the requested
/// range is free.
pub fn fcntl_getlk(fd: c_int, arg: usize, ofd: bool) -> AxResult<isize> {
    let fl = UserPtr::<flock64>::from(arg).get_as_mut()?;
    if fl.l_whence as u32 != SEEK_SET {
        return Err(AxError::InvalidInput);
    }
    let req_kind = match fl.l_type as u32 {
        F_RDLCK => LockKind::Read,
        F_WRLCK => LockKind::Write,
        _ => return Err(AxError::InvalidInput),
    };
    let (start, end) = flock_range(fl.l_start, fl.l_len)?;
    let (key, file) = lockable(fd)?;

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
                if e.end == i64::MAX { 0 } else { e.end - e.start },
            )
        });
        (report, entries.is_empty())
    };
    if empty_after {
        table.remove(&key);
    }

    if let Some((kind, pid, l_start, l_len)) = report {
        fl.l_type = (if kind == LockKind::Read { F_RDLCK } else { F_WRLCK }) as i16;
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

// ─── flock(2) ──────────────────────────────────────────────────────────

/// Implementation of `sys_flock`. Supports `LOCK_SH`, `LOCK_EX`, `LOCK_UN`,
/// optionally OR'd with `LOCK_NB`. Conflicting requests return
/// `EWOULDBLOCK` (== `EAGAIN`); we never block.
pub fn flock_op(fd: c_int, operation: c_int) -> AxResult<isize> {
    let op = operation as u32;
    // We always behave as non-blocking, so the LOCK_NB bit has no effect
    // on this implementation; we still strip it before matching.
    let kind = match op & !LOCK_NB {
        LOCK_SH => Some(LockKind::Read),
        LOCK_EX => Some(LockKind::Write),
        LOCK_UN => None,
        _ => return Err(AxError::InvalidInput),
    };

    let (key, file) = lockable(fd)?;
    let addr = ofd_addr(&file);

    let mut table = FLOCK_LOCKS.write();
    let (conflict, empty_after) = {
        let entries = table.entry(key).or_default();
        entries.retain(|e| e.weak.strong_count() != 0);

        let conflict = match kind {
            None => {
                // LOCK_UN: drop any entry held by this OFD.
                entries.retain(|e| e.addr != addr);
                false
            }
            Some(want) => {
                // For upgrade/downgrade, drop our own entry first so we
                // don't conflict with ourselves.
                entries.retain(|e| e.addr != addr);
                if entries.iter().any(|e| kinds_conflict(e.kind, want)) {
                    true
                } else {
                    entries.push(FlockEntry {
                        addr,
                        weak: Arc::downgrade(&file),
                        kind: want,
                    });
                    false
                }
            }
        };
        (conflict, entries.is_empty())
    };
    if empty_after {
        table.remove(&key);
    }
    if conflict {
        return Err(AxError::WouldBlock);
    }
    Ok(0)
}
