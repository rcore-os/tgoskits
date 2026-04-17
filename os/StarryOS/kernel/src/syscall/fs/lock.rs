//! File lock implementations for `flock(2)` and `fcntl(2)` POSIX record locks.
//!
//! A single global table is keyed by `(dev, ino)`. Each entry tracks:
//! - `flocks`: `flock(2)` holders, identified by their open-file description
//!   (the `Arc<dyn FileLike>` pointer).
//! - `posix`: `fcntl(2)` range locks, owned by a PID.
//! - `wake`: a [`PollSet`] used by blocking callers (`flock` without `LOCK_NB`
//!   and `fcntl F_SETLKW`) to wait until a conflicting lock is released.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::{ffi::c_int, future::poll_fn, task::Poll};

use ax_errno::{AxError, AxResult};
use ax_sync::Mutex;
use ax_task::{
    current,
    future::{block_on, interruptible},
};
use axpoll::PollSet;
use linux_raw_sys::general::{
    F_RDLCK, F_UNLCK, F_WRLCK, LOCK_EX, LOCK_NB, LOCK_SH, LOCK_UN, SEEK_CUR, SEEK_END, SEEK_SET,
    flock64,
};
use starry_process::Pid;

use crate::{file::FileLike, task::AsThread};

type InodeKey = (u64, u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LockKind {
    Shared,
    Exclusive,
}

#[derive(Debug)]
struct FlockEntry {
    ofd: usize,
    kind: LockKind,
}

#[derive(Debug, Clone)]
struct PosixLock {
    pid: Pid,
    kind: LockKind,
    start: u64,
    /// Exclusive upper bound. `u64::MAX` represents "to end of file".
    end: u64,
}

#[derive(Default)]
struct LockState {
    flocks: Vec<FlockEntry>,
    posix: Vec<PosixLock>,
    wake: Arc<PollSet>,
}

static LOCK_TABLE: Mutex<BTreeMap<InodeKey, LockState>> = Mutex::new(BTreeMap::new());

fn inode_key(f: &Arc<dyn FileLike>) -> AxResult<InodeKey> {
    let st = f.stat()?;
    Ok((st.dev, st.ino))
}

fn ofd_id(f: &Arc<dyn FileLike>) -> usize {
    // `Arc::as_ptr` returns a fat pointer to the trait object; casting
    // it to a thin pointer yields the address of the underlying allocation,
    // which is unique per open-file description.
    Arc::as_ptr(f) as *const u8 as usize
}

fn current_pid() -> Pid {
    current().as_thread().proc_data.proc.pid()
}

fn ranges_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
    a_start < b_end && b_start < a_end
}

/// Compute (start, end) in absolute file offsets from a `flock64` struct.
fn resolve_range(f: &Arc<dyn FileLike>, flk: &flock64) -> AxResult<(u64, u64)> {
    let base = match flk.l_whence as u32 {
        SEEK_SET => 0i64,
        SEEK_CUR => 0i64, // current file offset isn't tracked at this layer
        SEEK_END => f.stat()?.size as i64,
        _ => return Err(AxError::InvalidInput),
    };
    let start = base.checked_add(flk.l_start).ok_or(AxError::InvalidInput)?;
    if start < 0 {
        return Err(AxError::InvalidInput);
    }
    let start = start as u64;
    let end = if flk.l_len == 0 {
        u64::MAX
    } else if flk.l_len > 0 {
        start
            .checked_add(flk.l_len as u64)
            .ok_or(AxError::InvalidInput)?
    } else {
        // Negative length: range is [start + len, start).
        let new_start = (start as i64)
            .checked_add(flk.l_len)
            .ok_or(AxError::InvalidInput)?;
        if new_start < 0 {
            return Err(AxError::InvalidInput);
        }
        let old = start;
        let start = new_start as u64;
        return Ok((start, old));
    };
    Ok((start, end))
}

fn posix_kind(l_type: i16) -> AxResult<Option<LockKind>> {
    match l_type as u32 {
        F_RDLCK => Ok(Some(LockKind::Shared)),
        F_WRLCK => Ok(Some(LockKind::Exclusive)),
        F_UNLCK => Ok(None),
        _ => Err(AxError::InvalidInput),
    }
}

/// Find the first POSIX lock held by a *different* process that conflicts
/// with the requested range and kind. Same-PID holders never conflict with
/// themselves (POSIX F_GETLK semantics).
fn find_posix_conflict<'a>(
    st: &'a LockState,
    pid: Pid,
    kind: LockKind,
    start: u64,
    end: u64,
) -> Option<&'a PosixLock> {
    st.posix.iter().find(|l| {
        l.pid != pid
            && ranges_overlap(l.start, l.end, start, end)
            && (kind == LockKind::Exclusive || l.kind == LockKind::Exclusive)
    })
}

/// `flock(2)` implementation.
pub fn flock_op(f: &Arc<dyn FileLike>, operation: c_int) -> AxResult<isize> {
    let nb = (operation & LOCK_NB as c_int) != 0;
    let op = (operation & !(LOCK_NB as c_int)) as u32;
    let ofd = ofd_id(f);
    let key = inode_key(f)?;

    if op == LOCK_UN {
        let mut table = LOCK_TABLE.lock();
        if let Some(st) = table.get_mut(&key) {
            st.flocks.retain(|e| e.ofd != ofd);
            st.wake.wake();
            if st.flocks.is_empty() && st.posix.is_empty() {
                table.remove(&key);
            }
        }
        return Ok(0);
    }

    let want = match op {
        LOCK_SH => LockKind::Shared,
        LOCK_EX => LockKind::Exclusive,
        _ => return Err(AxError::InvalidInput),
    };

    let wake = LOCK_TABLE.lock().entry(key).or_default().wake.clone();
    let r = block_on(interruptible(poll_fn(|cx| {
        // Register first so a concurrent `wake()` cannot be lost between the
        // conflict check below and going to sleep.
        wake.register(cx.waker());

        let mut table = LOCK_TABLE.lock();
        let st = table.entry(key).or_default();
        let conflict = st.flocks.iter().any(|e| {
            e.ofd != ofd && (want == LockKind::Exclusive || e.kind == LockKind::Exclusive)
        });
        if !conflict {
            if let Some(existing) = st.flocks.iter_mut().find(|e| e.ofd == ofd) {
                existing.kind = want;
            } else {
                st.flocks.push(FlockEntry { ofd, kind: want });
            }
            return Poll::Ready(Ok(0));
        }
        if nb {
            return Poll::Ready(Err(AxError::WouldBlock));
        }
        Poll::Pending
    })));
    match r {
        Ok(res) => res,
        Err(_) => Err(AxError::Interrupted),
    }
}

/// Drop flock entries owned by an OFD when the last reference to the file
/// is closed.
pub fn flock_release_ofd(f: &Arc<dyn FileLike>) {
    let Ok(key) = inode_key(f) else {
        return;
    };
    let ofd = ofd_id(f);
    let mut table = LOCK_TABLE.lock();
    if let Some(st) = table.get_mut(&key) {
        st.flocks.retain(|e| e.ofd != ofd);
        st.wake.wake();
        if st.flocks.is_empty() && st.posix.is_empty() {
            table.remove(&key);
        }
    }
}

/// Replace the range `[start, end)` among same-PID POSIX locks.
///
/// This is a simplified split/merge: locks that fall entirely inside the
/// target range are removed; locks that straddle a boundary are truncated.
fn remove_posix_range(st: &mut LockState, pid: Pid, start: u64, end: u64) {
    let mut new = Vec::with_capacity(st.posix.len());
    for l in st.posix.drain(..) {
        if l.pid != pid || !ranges_overlap(l.start, l.end, start, end) {
            new.push(l);
            continue;
        }
        // Keep portion before `start`.
        if l.start < start {
            new.push(PosixLock {
                pid: l.pid,
                kind: l.kind,
                start: l.start,
                end: start,
            });
        }
        // Keep portion after `end`.
        if l.end > end {
            new.push(PosixLock {
                pid: l.pid,
                kind: l.kind,
                start: end,
                end: l.end,
            });
        }
    }
    st.posix = new;
}

/// `fcntl(fd, F_GETLK, flk)` — fill `flk.l_type = F_UNLCK` if the request
/// would succeed, otherwise describe the conflicting lock.
pub fn fcntl_getlk(f: &Arc<dyn FileLike>, flk: &mut flock64) -> AxResult<isize> {
    let Some(want) = posix_kind(flk.l_type)? else {
        return Err(AxError::InvalidInput);
    };
    let (start, end) = resolve_range(f, flk)?;
    let key = inode_key(f)?;
    let pid = current_pid();

    let table = LOCK_TABLE.lock();
    if let Some(st) = table.get(&key)
        && let Some(conflict) = find_posix_conflict(st, pid, want, start, end)
    {
        flk.l_type = match conflict.kind {
            LockKind::Shared => F_RDLCK as _,
            LockKind::Exclusive => F_WRLCK as _,
        };
        flk.l_whence = SEEK_SET as _;
        flk.l_start = conflict.start as i64;
        flk.l_len = if conflict.end == u64::MAX {
            0
        } else {
            (conflict.end - conflict.start) as i64
        };
        flk.l_pid = conflict.pid as _;
    } else {
        flk.l_type = F_UNLCK as _;
    }
    Ok(0)
}

/// `fcntl(fd, F_SETLK|F_SETLKW, flk)`.
pub fn fcntl_setlk(f: &Arc<dyn FileLike>, flk: &flock64, blocking: bool) -> AxResult<isize> {
    let kind = posix_kind(flk.l_type)?;
    let (start, end) = resolve_range(f, flk)?;
    let key = inode_key(f)?;
    let pid = current_pid();

    match kind {
        None => {
            let mut table = LOCK_TABLE.lock();
            if let Some(st) = table.get_mut(&key) {
                remove_posix_range(st, pid, start, end);
                st.wake.wake();
                if st.flocks.is_empty() && st.posix.is_empty() {
                    table.remove(&key);
                }
            }
            Ok(0)
        }
        Some(want) => {
            let wake = LOCK_TABLE.lock().entry(key).or_default().wake.clone();
            let r = block_on(interruptible(poll_fn(|cx| {
                // Register the waker before inspecting the table so a
                // concurrent `wake()` cannot slip through.
                wake.register(cx.waker());

                let mut table = LOCK_TABLE.lock();
                let st = table.entry(key).or_default();
                if find_posix_conflict(st, pid, want, start, end).is_none() {
                    remove_posix_range(st, pid, start, end);
                    st.posix.push(PosixLock {
                        pid,
                        kind: want,
                        start,
                        end,
                    });
                    return Poll::Ready(Ok(0));
                }
                if !blocking {
                    return Poll::Ready(Err(AxError::WouldBlock));
                }
                Poll::Pending
            })));
            match r {
                Ok(res) => res,
                Err(_) => Err(AxError::Interrupted),
            }
        }
    }
}

/// Release all POSIX locks owned by the given PID (called on process exit).
#[allow(dead_code)]
pub fn release_posix_locks_for_pid(pid: Pid) {
    let mut table = LOCK_TABLE.lock();
    let mut empty_keys = Vec::new();
    for (key, st) in table.iter_mut() {
        let before = st.posix.len();
        st.posix.retain(|l| l.pid != pid);
        if st.posix.len() != before {
            st.wake.wake();
        }
        if st.flocks.is_empty() && st.posix.is_empty() {
            empty_keys.push(*key);
        }
    }
    for k in empty_keys {
        table.remove(&k);
    }
}
