use alloc::{sync::Arc, vec, vec::Vec};
use core::{ffi::c_char, mem::MaybeUninit};

use ax_config::ARCH;
use ax_errno::{AxError, AxResult};
use ax_fs::FS_CONTEXT;
use ax_sync::Mutex;
use ax_task::current;
use linux_raw_sys::{
    general::{GRND_INSECURE, GRND_NONBLOCK, GRND_RANDOM},
    system::{new_utsname, sysinfo},
};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer},
};
use starry_vm::{VmMutPtr, vm_read_slice, vm_write_slice};
use syscalls::Sysno;

#[cfg(target_arch = "riscv64")]
use crate::mm::UserPtr;
use crate::task::{AsThread, SeccompFilterData, SeccompInsn, processes};

/// Sentinel value meaning "don't change this ID" (userspace passes -1 as signed,
/// which becomes `u32::MAX` after the `as u32` cast in the dispatch table).
///
/// Note: paired with `uid_valid()` below — multi-arg `set*res*uid/gid` and
/// `setre*uid/gid` use this sentinel for NOCHG semantics, while single-arg
/// `setuid/setgid` reject it as EINVAL (no NOCHG slot exists there).
const NOCHG: u32 = u32::MAX;
const SYSLOG_ACTION_CLOSE: i32 = 0;
const SYSLOG_ACTION_OPEN: i32 = 1;
const SYSLOG_ACTION_READ: i32 = 2;
const SYSLOG_ACTION_READ_ALL: i32 = 3;
const SYSLOG_ACTION_READ_CLEAR: i32 = 4;
const SYSLOG_ACTION_CLEAR: i32 = 5;
const SYSLOG_ACTION_CONSOLE_OFF: i32 = 6;
const SYSLOG_ACTION_CONSOLE_ON: i32 = 7;
const SYSLOG_ACTION_CONSOLE_LEVEL: i32 = 8;
const SYSLOG_ACTION_SIZE_UNREAD: i32 = 9;
const SYSLOG_ACTION_SIZE_BUFFER: i32 = 10;
const SYSLOG_BUFFER_CAPACITY: usize = 4096;
const SYSLOG_SEED_MESSAGE: &[u8] = b"StarryOS kernel log buffer initialized\n";

struct SyslogState {
    buffer: HeapRb<u8>,
    console_enabled: bool,
    console_level: usize,
}

impl SyslogState {
    fn new() -> Self {
        let mut buffer = HeapRb::new(SYSLOG_BUFFER_CAPACITY);
        buffer.push_slice(SYSLOG_SEED_MESSAGE);
        Self {
            buffer,
            console_enabled: true,
            console_level: 7,
        }
    }

    fn unread_len(&self) -> usize {
        self.buffer.occupied_len()
    }

    fn buffer_len(&self) -> usize {
        self.buffer.capacity().get()
    }

    fn read(&mut self, len: usize) -> Vec<u8> {
        let available = len.min(self.buffer.occupied_len());
        let (left, right) = self.buffer.as_slices();
        let mut out = Vec::with_capacity(available);
        let first = left.len().min(available);
        out.extend_from_slice(&left[..first]);
        if first < available {
            out.extend_from_slice(&right[..available - first]);
        }
        unsafe { self.buffer.advance_read_index(available) };
        out
    }

    fn read_all(&self, len: usize) -> Vec<u8> {
        let available = len.min(self.buffer.occupied_len());
        let (left, right) = self.buffer.as_slices();
        let mut out = Vec::with_capacity(available);
        let first = left.len().min(available);
        out.extend_from_slice(&left[..first]);
        if first < available {
            out.extend_from_slice(&right[..available - first]);
        }
        out
    }

    fn clear(&mut self) {
        let len = self.buffer.occupied_len();
        unsafe { self.buffer.advance_read_index(len) };
    }
}

static SYSLOG_STATE: spin::LazyLock<Mutex<SyslogState>> =
    spin::LazyLock::new(|| Mutex::new(SyslogState::new()));

/// Mirror of Linux kernel `uid_valid()` / `make_kuid()` rejection: any caller-
/// supplied UID/GID of `(uid_t)-1` (`u32::MAX`) is invalid outside the NOCHG
/// sentinel slots of multi-arg setters. Single-arg `setuid`/`setgid` have no
/// NOCHG semantic, so they must always reject `u32::MAX` with `EINVAL` before
/// touching `cred` — otherwise a malicious caller writes the sentinel into
/// real / effective / saved IDs and the next `setresuid` NOCHG path silently
/// no-ops on already-poisoned credentials.
fn uid_valid(id: u32) -> bool {
    id != NOCHG
}

/// Linux clears `mm->dumpable` from `commit_creds()` when effective or
/// filesystem credentials change. StarryOS keeps this process-wide flag on
/// `ProcessData`, so each credential setter checks the committed deltas.
#[inline]
fn dumpable_should_reset(old: &crate::task::Cred, new: &crate::task::Cred) -> bool {
    old.euid != new.euid || old.egid != new.egid || old.fsuid != new.fsuid || old.fsgid != new.fsgid
}

pub fn sys_getuid() -> AxResult<isize> {
    let cred = current().as_thread().cred();
    Ok(cred.uid as isize)
}

pub fn sys_geteuid() -> AxResult<isize> {
    let cred = current().as_thread().cred();
    Ok(cred.euid as isize)
}

pub fn sys_getgid() -> AxResult<isize> {
    let cred = current().as_thread().cred();
    Ok(cred.gid as isize)
}

pub fn sys_getegid() -> AxResult<isize> {
    let cred = current().as_thread().cred();
    Ok(cred.egid as isize)
}

pub fn sys_getresuid(ruid: *mut u32, euid: *mut u32, suid: *mut u32) -> AxResult<isize> {
    let cred = current().as_thread().cred();
    ruid.vm_write(cred.uid)?;
    euid.vm_write(cred.euid)?;
    suid.vm_write(cred.suid)?;
    Ok(0)
}

pub fn sys_getresgid(rgid: *mut u32, egid: *mut u32, sgid: *mut u32) -> AxResult<isize> {
    let cred = current().as_thread().cred();
    rgid.vm_write(cred.gid)?;
    egid.vm_write(cred.egid)?;
    sgid.vm_write(cred.sgid)?;
    Ok(0)
}

// ── setresuid / setresgid ────────────────────────────────────────────

pub fn sys_setresuid(ruid: u32, euid: u32, suid: u32) -> AxResult<isize> {
    debug!("sys_setresuid <= ruid: {ruid}, euid: {euid}, suid: {suid}");
    let thread = current();
    let thread = thread.as_thread();
    let old = thread.cred();
    let mut new = (*old).clone();

    if old.has_cap_setuid() {
        // Privileged: arbitrary values allowed.
        if ruid != NOCHG {
            new.uid = ruid;
        }
        if euid != NOCHG {
            new.euid = euid;
        }
        if suid != NOCHG {
            new.suid = suid;
        }
    } else {
        // Unprivileged: each new value must be one of {uid, euid, suid}.
        let allowed = [old.uid, old.euid, old.suid];
        if ruid != NOCHG {
            if !allowed.contains(&ruid) {
                return Err(AxError::OperationNotPermitted);
            }
            new.uid = ruid;
        }
        if euid != NOCHG {
            if !allowed.contains(&euid) {
                return Err(AxError::OperationNotPermitted);
            }
            new.euid = euid;
        }
        if suid != NOCHG {
            if !allowed.contains(&suid) {
                return Err(AxError::OperationNotPermitted);
            }
            new.suid = suid;
        }
    }

    // fsuid always tracks euid.
    new.fsuid = new.euid;
    let reset_dumpable = dumpable_should_reset(&old, &new);
    thread.set_cred(new);
    if reset_dumpable {
        thread.proc_data.set_dumpable(0);
    }
    Ok(0)
}

pub fn sys_setresgid(rgid: u32, egid: u32, sgid: u32) -> AxResult<isize> {
    debug!("sys_setresgid <= rgid: {rgid}, egid: {egid}, sgid: {sgid}");
    let thread = current();
    let thread = thread.as_thread();
    let old = thread.cred();
    let mut new = (*old).clone();

    if old.has_cap_setgid() {
        if rgid != NOCHG {
            new.gid = rgid;
        }
        if egid != NOCHG {
            new.egid = egid;
        }
        if sgid != NOCHG {
            new.sgid = sgid;
        }
    } else {
        let allowed = [old.gid, old.egid, old.sgid];
        if rgid != NOCHG {
            if !allowed.contains(&rgid) {
                return Err(AxError::OperationNotPermitted);
            }
            new.gid = rgid;
        }
        if egid != NOCHG {
            if !allowed.contains(&egid) {
                return Err(AxError::OperationNotPermitted);
            }
            new.egid = egid;
        }
        if sgid != NOCHG {
            if !allowed.contains(&sgid) {
                return Err(AxError::OperationNotPermitted);
            }
            new.sgid = sgid;
        }
    }

    new.fsgid = new.egid;
    let reset_dumpable = dumpable_should_reset(&old, &new);
    thread.set_cred(new);
    if reset_dumpable {
        thread.proc_data.set_dumpable(0);
    }
    Ok(0)
}

// ── setuid / setgid ─────────────────────────────────────────────────

pub fn sys_setuid(uid: u32) -> AxResult<isize> {
    debug!("sys_setuid <= uid: {uid}");
    // Linux setuid(2) §ERRORS: "EINVAL — uid is not valid in this user namespace."
    // Single-arg setuid has no NOCHG sentinel; (uid_t)-1 must be rejected.
    if !uid_valid(uid) {
        return Err(AxError::InvalidInput);
    }
    let thread = current();
    let thread = thread.as_thread();
    let old = thread.cred();
    let mut new = (*old).clone();

    if old.has_cap_setuid() {
        // Privileged: sets uid, euid, suid ALL (irreversible).
        new.uid = uid;
        new.euid = uid;
        new.suid = uid;
    } else {
        // Unprivileged: only sets euid, and only if uid matches uid or suid.
        if uid != old.uid && uid != old.suid {
            return Err(AxError::OperationNotPermitted);
        }
        new.euid = uid;
    }

    new.fsuid = new.euid;
    let reset_dumpable = dumpable_should_reset(&old, &new);
    thread.set_cred(new);
    if reset_dumpable {
        thread.proc_data.set_dumpable(0);
    }
    Ok(0)
}

pub fn sys_setgid(gid: u32) -> AxResult<isize> {
    debug!("sys_setgid <= gid: {gid}");
    // Linux setgid(2) §ERRORS: "EINVAL — gid is not valid in this user namespace."
    if !uid_valid(gid) {
        return Err(AxError::InvalidInput);
    }
    let thread = current();
    let thread = thread.as_thread();
    let old = thread.cred();
    let mut new = (*old).clone();

    if old.has_cap_setgid() {
        new.gid = gid;
        new.egid = gid;
        new.sgid = gid;
    } else {
        if gid != old.gid && gid != old.sgid {
            return Err(AxError::OperationNotPermitted);
        }
        new.egid = gid;
    }

    new.fsgid = new.egid;
    let reset_dumpable = dumpable_should_reset(&old, &new);
    thread.set_cred(new);
    if reset_dumpable {
        thread.proc_data.set_dumpable(0);
    }
    Ok(0)
}

// ── setreuid / setregid ─────────────────────────────────────────────

pub fn sys_setreuid(ruid: u32, euid: u32) -> AxResult<isize> {
    debug!("sys_setreuid <= ruid: {ruid}, euid: {euid}");
    let thread = current();
    let thread = thread.as_thread();
    let old = thread.cred();
    let mut new = (*old).clone();

    if old.has_cap_setuid() {
        if ruid != NOCHG {
            new.uid = ruid;
        }
        if euid != NOCHG {
            new.euid = euid;
        }
    } else {
        // ruid can only be set to current uid or euid.
        if ruid != NOCHG {
            if ruid != old.uid && ruid != old.euid {
                return Err(AxError::OperationNotPermitted);
            }
            new.uid = ruid;
        }
        // euid can be set to current uid, euid, or suid.
        if euid != NOCHG {
            if euid != old.uid && euid != old.euid && euid != old.suid {
                return Err(AxError::OperationNotPermitted);
            }
            new.euid = euid;
        }
    }

    // Per setreuid(2) man page: "If the real user ID is set (i.e.,
    // ruid is not -1) or the effective user ID is set to a value not
    // equal to the previous real user ID, the saved set-user-ID will
    // be set to the new effective user ID."
    if ruid != NOCHG || (euid != NOCHG && new.euid != old.uid) {
        new.suid = new.euid;
    }

    new.fsuid = new.euid;
    let reset_dumpable = dumpable_should_reset(&old, &new);
    thread.set_cred(new);
    if reset_dumpable {
        thread.proc_data.set_dumpable(0);
    }
    Ok(0)
}

pub fn sys_setregid(rgid: u32, egid: u32) -> AxResult<isize> {
    debug!("sys_setregid <= rgid: {rgid}, egid: {egid}");
    let thread = current();
    let thread = thread.as_thread();
    let old = thread.cred();
    let mut new = (*old).clone();

    if old.has_cap_setgid() {
        if rgid != NOCHG {
            new.gid = rgid;
        }
        if egid != NOCHG {
            new.egid = egid;
        }
    } else {
        if rgid != NOCHG {
            if rgid != old.gid && rgid != old.egid {
                return Err(AxError::OperationNotPermitted);
            }
            new.gid = rgid;
        }
        if egid != NOCHG {
            if egid != old.gid && egid != old.egid && egid != old.sgid {
                return Err(AxError::OperationNotPermitted);
            }
            new.egid = egid;
        }
    }

    if rgid != NOCHG || (egid != NOCHG && new.egid != old.gid) {
        new.sgid = new.egid;
    }

    new.fsgid = new.egid;
    let reset_dumpable = dumpable_should_reset(&old, &new);
    thread.set_cred(new);
    if reset_dumpable {
        thread.proc_data.set_dumpable(0);
    }
    Ok(0)
}

// ── setfsuid / setfsgid ─────────────────────────────────────────────
//
// man 2 setfsuid:
//   "setfsuid() sets the user ID that the Linux kernel uses to check for all
//    accesses to the filesystem. ... On both success and failure, this call
//    returns the previous filesystem user ID of the caller."
//   "When the effective user ID is changed (via setuid(), setresuid(), etc.),
//    the kernel also changes the filesystem user ID to the new value of the
//    effective user ID."
//   Query trick: passing `(uid_t)-1` leaves the fsuid unchanged but still
//   returns the previous value — used by libc to read the current fsuid.

pub fn sys_setfsuid(fsuid: u32) -> AxResult<isize> {
    debug!("sys_setfsuid <= fsuid: {fsuid}");
    let thread = current();
    let thread = thread.as_thread();
    let old = thread.cred();
    let prev_fsuid = old.fsuid;

    // (uid_t)-1 = query-only: don't change, just return prev.
    if fsuid == NOCHG {
        return Ok(prev_fsuid as isize);
    }

    // Linux: setfsuid silently ignores an invalid fsuid but always returns the
    // previous fsuid (never reports error). Unprivileged callers may only set
    // fsuid to one of {uid, euid, suid, fsuid}; CAP_SETUID allows arbitrary.
    let allowed = old.has_cap_setuid()
        || fsuid == old.uid
        || fsuid == old.euid
        || fsuid == old.suid
        || fsuid == old.fsuid;

    if allowed {
        let mut new = (*old).clone();
        new.fsuid = fsuid;
        let reset_dumpable = dumpable_should_reset(&old, &new);
        thread.set_cred(new);
        if reset_dumpable {
            thread.proc_data.set_dumpable(0);
        }
    }
    // Always return previous fsuid, even when the request was ignored.
    Ok(prev_fsuid as isize)
}

pub fn sys_setfsgid(fsgid: u32) -> AxResult<isize> {
    debug!("sys_setfsgid <= fsgid: {fsgid}");
    let thread = current();
    let thread = thread.as_thread();
    let old = thread.cred();
    let prev_fsgid = old.fsgid;

    if fsgid == NOCHG {
        return Ok(prev_fsgid as isize);
    }

    let allowed = old.has_cap_setgid()
        || fsgid == old.gid
        || fsgid == old.egid
        || fsgid == old.sgid
        || fsgid == old.fsgid;

    if allowed {
        let mut new = (*old).clone();
        new.fsgid = fsgid;
        let reset_dumpable = dumpable_should_reset(&old, &new);
        thread.set_cred(new);
        if reset_dumpable {
            thread.proc_data.set_dumpable(0);
        }
    }
    Ok(prev_fsgid as isize)
}

pub fn sys_getgroups(size: usize, list: *mut u32) -> AxResult<isize> {
    debug!("sys_getgroups <= size: {size}");
    let cred = current().as_thread().cred();
    let ngroups = cred.groups.len();
    if size == 0 {
        return Ok(ngroups as isize);
    }
    if size < ngroups {
        return Err(AxError::InvalidInput);
    }
    if ngroups > 0 {
        vm_write_slice(list, &cred.groups)?;
    }
    Ok(ngroups as isize)
}

/// Linux limits supplementary groups to 65536 (`NGROUPS_MAX`).
const NGROUPS_MAX: usize = 65536;

pub fn sys_setgroups(size: usize, list: *const u32) -> AxResult<isize> {
    debug!("sys_setgroups <= size: {size}");
    let thread = current();
    let thread = thread.as_thread();
    let old = thread.cred();

    if !old.has_cap_setgid() {
        return Err(AxError::OperationNotPermitted);
    }
    if size > NGROUPS_MAX {
        return Err(AxError::InvalidInput);
    }

    let groups = if size > 0 {
        let mut buf: Vec<MaybeUninit<u32>> = vec![MaybeUninit::uninit(); size];
        vm_read_slice(list, &mut buf)?;
        // SAFETY: vm_read_slice filled all elements with data from user space.
        buf.into_iter()
            .map(|v| unsafe { v.assume_init() })
            .collect()
    } else {
        Vec::new()
    };

    let mut new = (*old).clone();
    new.groups = Arc::from(groups.into_boxed_slice());
    thread.set_cred(new);
    Ok(0)
}

const fn pad_str(info: &str) -> [c_char; 65] {
    let mut data: [c_char; 65] = [0; 65];
    // this needs #![feature(const_copy_from_slice)]
    // data[..info.len()].copy_from_slice(info.as_bytes());
    unsafe {
        core::ptr::copy_nonoverlapping(info.as_ptr().cast(), data.as_mut_ptr(), info.len());
    }
    data
}

const UTSNAME: new_utsname = new_utsname {
    sysname: pad_str("Linux"),
    nodename: pad_str("starry"),
    release: pad_str("10.0.0"),
    version: pad_str("10.0.0"),
    machine: pad_str(ARCH),
    domainname: pad_str("https://github.com/Starry-OS/StarryOS"),
};

pub fn sys_uname(name: *mut new_utsname) -> AxResult<isize> {
    name.vm_write(UTSNAME)?;
    Ok(0)
}

pub fn sys_sysinfo(info: *mut sysinfo) -> AxResult<isize> {
    let mut kinfo: sysinfo = unsafe { core::mem::zeroed() };

    let total = ax_runtime::hal::mem::total_ram_size();
    let usages = ax_alloc::global_allocator().usages();
    let used = usages.get(ax_alloc::UsageKind::RustHeap)
        + usages.get(ax_alloc::UsageKind::VirtMem)
        + usages.get(ax_alloc::UsageKind::PageCache)
        + usages.get(ax_alloc::UsageKind::PageTable)
        + usages.get(ax_alloc::UsageKind::Dma)
        + usages.get(ax_alloc::UsageKind::Global);
    let free = total.saturating_sub(used);
    let uptime = ax_runtime::hal::time::monotonic_time();

    kinfo.uptime = uptime.as_secs() as _;
    kinfo.totalram = total as _;
    kinfo.freeram = free as _;
    kinfo.procs = processes().len() as _;
    kinfo.mem_unit = 1;

    info.vm_write(kinfo)?;
    Ok(0)
}

fn require_syslog_privilege() -> AxResult<()> {
    if current().as_thread().cred().euid == 0 {
        Ok(())
    } else {
        Err(AxError::OperationNotPermitted)
    }
}

pub fn sys_syslog(ty: i32, buf: *mut c_char, len: usize) -> AxResult<isize> {
    match ty {
        SYSLOG_ACTION_CLOSE | SYSLOG_ACTION_OPEN => Ok(0),
        SYSLOG_ACTION_READ => {
            require_syslog_privilege()?;
            let data = {
                let mut state = SYSLOG_STATE.lock();
                state.read(len)
            };
            if !data.is_empty() {
                vm_write_slice(buf.cast::<u8>(), &data)?;
            }
            Ok(data.len() as isize)
        }
        SYSLOG_ACTION_READ_ALL => {
            require_syslog_privilege()?;
            let data = {
                let state = SYSLOG_STATE.lock();
                state.read_all(len)
            };
            if !data.is_empty() {
                vm_write_slice(buf.cast::<u8>(), &data)?;
            }
            Ok(data.len() as isize)
        }
        SYSLOG_ACTION_READ_CLEAR => {
            require_syslog_privilege()?;
            let data = {
                let mut state = SYSLOG_STATE.lock();
                let data = state.read_all(len);
                state.clear();
                data
            };
            if !data.is_empty() {
                vm_write_slice(buf.cast::<u8>(), &data)?;
            }
            Ok(data.len() as isize)
        }
        SYSLOG_ACTION_CLEAR => {
            require_syslog_privilege()?;
            let mut state = SYSLOG_STATE.lock();
            state.clear();
            Ok(0)
        }
        SYSLOG_ACTION_CONSOLE_OFF => {
            require_syslog_privilege()?;
            let mut state = SYSLOG_STATE.lock();
            state.console_enabled = false;
            Ok(0)
        }
        SYSLOG_ACTION_CONSOLE_ON => {
            require_syslog_privilege()?;
            let mut state = SYSLOG_STATE.lock();
            state.console_enabled = true;
            Ok(0)
        }
        SYSLOG_ACTION_CONSOLE_LEVEL => {
            require_syslog_privilege()?;
            if !(1..=8).contains(&len) {
                return Err(AxError::InvalidInput);
            }
            let mut state = SYSLOG_STATE.lock();
            let old_level = state.console_level;
            state.console_level = len;
            Ok(old_level as isize)
        }
        SYSLOG_ACTION_SIZE_UNREAD => {
            require_syslog_privilege()?;
            let state = SYSLOG_STATE.lock();
            Ok(state.unread_len() as isize)
        }
        SYSLOG_ACTION_SIZE_BUFFER => {
            let state = SYSLOG_STATE.lock();
            Ok(state.buffer_len() as isize)
        }
        _ => Err(AxError::InvalidInput),
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct GetRandomFlags: u32 {
        const NONBLOCK = GRND_NONBLOCK;
        const RANDOM = GRND_RANDOM;
        const INSECURE = GRND_INSECURE;
    }
}

pub fn sys_getrandom(buf: *mut u8, len: usize, flags: u32) -> AxResult<isize> {
    if len == 0 {
        return Ok(0);
    }
    let flags = GetRandomFlags::from_bits(flags).ok_or(AxError::InvalidInput)?;
    if flags.contains(GetRandomFlags::INSECURE) && flags.contains(GetRandomFlags::RANDOM) {
        return Err(AxError::InvalidInput);
    }

    debug!("sys_getrandom <= buf: {buf:p}, len: {len}, flags: {flags:?}");

    let path = if flags.contains(GetRandomFlags::RANDOM) {
        "/dev/random"
    } else {
        "/dev/urandom"
    };

    let f = FS_CONTEXT.lock().resolve(path)?;
    let mut kbuf = vec![0; len];
    let len = f.entry().as_file()?.read_at(&mut kbuf, 0)?;

    vm_write_slice(buf, &kbuf)?;

    Ok(len as _)
}

// ---------------------------------------------------------------------------
// seccomp(2)
// ---------------------------------------------------------------------------

/// seccomp(2) operations.
mod seccomp_op {
    pub const SET_MODE_STRICT: u32 = 0;
    pub const SET_MODE_FILTER: u32 = 1;
    pub const GET_ACTION_AVAIL: u32 = 2;
    pub const GET_NOTIF_SIZES: u32 = 3;
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SeccompFilterFlags: u32 {
        const TSYNC = 1 << 0;
        const LOG = 1 << 1;
        const SPEC_ALLOW = 1 << 2;
        const NEW_LISTENER = 1 << 3;
    }
}

/// seccomp(2) action / return values.
pub(crate) mod seccomp_ret {
    pub const KILL_THREAD: u32 = 0x00000000;
    pub const KILL_PROCESS: u32 = 0x80000000;
    pub const TRAP: u32 = 0x00030000;
    pub const ERRNO: u32 = 0x00050000;
    pub const TRACE: u32 = 0x7ff00000;
    pub const LOG: u32 = 0x7ffc0000;
    pub const ALLOW: u32 = 0x7fff0000;

    pub const ACTION_FULL: u32 = 0xffff0000;
    #[allow(dead_code)]
    pub const DATA: u32 = 0x0000ffff;
}

/// seccomp(2) mode values stored in Thread.
pub(crate) mod seccomp_mode {
    pub const DISABLED: u32 = 0;
    pub const STRICT: u32 = 1;
    pub const FILTER: u32 = 2;
}

/// Architecture constant written into `seccomp_data.arch` so that BPF
/// filters can match `AUDIT_ARCH_*` to validate the calling ABI.
/// Only referenced when the `ebpf` feature is enabled.
#[allow(dead_code)]
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E;
#[allow(dead_code)]
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7;
#[allow(dead_code)]
#[cfg(target_arch = "riscv64")]
const AUDIT_ARCH: u32 = 0xC000_00F3;
#[allow(dead_code)]
#[cfg(target_arch = "loongarch64")]
const AUDIT_ARCH: u32 = 0xC000_0102;

/// The `seccomp_data` buffer passed to BPF filter programs.
///
/// Layout matches Linux's `struct seccomp_data` (64 bytes on 64-bit):
///   offset  size   field
///     0      4    nr                    (syscall number, i32)
///     4      4    arch                  (AUDIT_ARCH_*, u32)
///     8      8    instruction_pointer   (u64)
///    16     48    args[6]               (six u64 syscall arguments)
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct SeccompData {
    /// Syscall number (e.g. `__NR_read`, `__NR_write`).
    pub nr: i32,
    /// Architecture token (`AUDIT_ARCH_X86_64`, …).
    pub arch: u32,
    /// User-space instruction pointer at the moment of the syscall.
    pub instruction_pointer: u64,
    /// Up to six syscall arguments as seen in the trap frame.
    pub args: [u64; 6],
}

// SAFETY: all fields are POD.
unsafe impl bytemuck::Zeroable for SeccompData {}
unsafe impl bytemuck::AnyBitPattern for SeccompData {}
unsafe impl bytemuck::NoUninit for SeccompData {}

impl SeccompData {
    #[allow(dead_code)]
    pub(crate) fn from_uctx(
        sysno: Sysno,
        uctx: &ax_runtime::hal::cpu::uspace::UserContext,
    ) -> Self {
        Self {
            nr: sysno as i32,
            arch: AUDIT_ARCH,
            instruction_pointer: uctx.ip() as u64,
            args: [
                uctx.arg0() as u64,
                uctx.arg1() as u64,
                uctx.arg2() as u64,
                uctx.arg3() as u64,
                uctx.arg4() as u64,
                uctx.arg5() as u64,
            ],
        }
    }
}

#[repr(C)]
struct SeccompNotifSizes {
    seccomp_notif: u16,
    seccomp_notif_resp: u16,
    seccomp_data: u16,
}

/// Userspace representation of `struct sock_fprog` passed to
/// `seccomp(SECCOMP_SET_MODE_FILTER)`.
///
/// `len` is the number of filter instructions; `filter_ptr` is a
/// userspace pointer to the first `struct sock_filter`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct SockFprog {
    /// Number of `sock_filter` instructions in the program.
    pub(crate) len: u16,
    /// Userspace pointer to the first `sock_filter` instruction.
    pub(crate) filter_ptr: usize,
}

// SAFETY: all fields are primitives.
unsafe impl bytemuck::Zeroable for SockFprog {}
unsafe impl bytemuck::AnyBitPattern for SockFprog {}

/// Alias for `SeccompInsn` used when reading the raw userspace
/// `struct sock_filter` array during `SECCOMP_SET_MODE_FILTER`.
type SockFilter = SeccompInsn;

#[cfg(feature = "ebpf")]
mod seccomp_ebpf_convert {
    use alloc::{vec, vec::Vec};

    use crate::{
        ebpf::bpf_insn::{self, BpfInsn},
        task::SeccompInsn,
    };

    /// Convert a classic BPF filter program to eBPF.
    ///
    /// cBPF register mapping: A → R0, X → R6.
    /// Scratch memory M[0..15] → stack slots at fp - 16 - k*4.
    pub fn cbpf_to_ebpf(insns: &[SeccompInsn]) -> Vec<BpfInsn> {
        assert!(!insns.is_empty(), "empty cBPF program");

        let n = insns.len();

        // --- pass 1: compute how many eBPF insns each cBPF insn becomes ---
        let mut expand = vec![0u8; n];
        for (pc, i) in insns.iter().enumerate() {
            let class = i.code & 0x07;
            expand[pc] = match class {
                0x05 if (i.code & 0xf0) != 0x00 => 2,
                0x05 => 1,
                0x06 if (i.code & 0x08) == 0 => 2,
                0x06 => 1,
                _ => 1,
            };
        }

        // --- build pc map: cBPF pc → starting eBPF pc ---
        let mut epc_map = vec![0u32; n + 1];
        for pc in 0..n {
            epc_map[pc + 1] = epc_map[pc] + expand[pc] as u32;
        }
        let total = epc_map[n] as usize;
        let mut out: Vec<BpfInsn> = Vec::with_capacity(total);

        // --- pass 2: emit eBPF instructions ---
        for (pc, i) in insns.iter().enumerate() {
            let code = i.code;
            let class = code & 0x07;
            let src = (code & 0x08) != 0;
            let epc = epc_map[pc] as usize;

            match class {
                0x00 => {
                    let mode = (code & 0xe0) as u8;
                    if mode == 0x00 {
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_ALU64 | bpf_insn::BPF_MOV | bpf_insn::BPF_K,
                            0,
                            0,
                            0,
                            i.k as i32,
                        ));
                    } else if mode == 0x20 || mode == 0x40 {
                        let size_code = (code & 0x18) as u8;
                        let bpf_size = match size_code {
                            0x00 => bpf_insn::BPF_W,
                            0x08 => bpf_insn::BPF_H,
                            0x10 => bpf_insn::BPF_B,
                            _ => bpf_insn::BPF_W,
                        };
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_LD | bpf_size | mode,
                            0,
                            0,
                            0,
                            i.k as i32,
                        ));
                    } else if mode == 0x60 {
                        let off = -(16i32 + (i.k & 0xf) as i32 * 4) as i16;
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_LDX | bpf_insn::BPF_MEM | bpf_insn::BPF_W,
                            0,
                            10,
                            off,
                            0,
                        ));
                    } else if mode == 0x80 {
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_LD | bpf_insn::BPF_LEN,
                            0,
                            0,
                            0,
                            0,
                        ));
                    } else {
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_ALU64 | bpf_insn::BPF_MOV | bpf_insn::BPF_K,
                            0,
                            0,
                            0,
                            0,
                        ));
                    }
                }
                0x01 => {
                    let mode = (code & 0xe0) as u8;
                    if mode == 0x00 {
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_ALU64 | bpf_insn::BPF_MOV | bpf_insn::BPF_K,
                            6,
                            0,
                            0,
                            i.k as i32,
                        ));
                    } else if mode == 0x60 {
                        let off = -(16i32 + (i.k & 0xf) as i32 * 4) as i16;
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_LDX | bpf_insn::BPF_MEM | bpf_insn::BPF_W,
                            6,
                            10,
                            off,
                            0,
                        ));
                    } else if mode == 0xa0 {
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_LDX | bpf_insn::BPF_MSH | bpf_insn::BPF_B,
                            6,
                            0,
                            0,
                            i.k as i32,
                        ));
                    } else {
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_ALU64 | bpf_insn::BPF_MOV | bpf_insn::BPF_K,
                            6,
                            0,
                            0,
                            0,
                        ));
                    }
                }
                0x02 | 0x03 => {
                    let src_reg = if class == 0x02 { 0u8 } else { 6u8 };
                    let off = -(16i32 + (i.k & 0xf) as i32 * 4) as i16;
                    out.push(BpfInsn::new(
                        bpf_insn::BPF_STX | bpf_insn::BPF_MEM | bpf_insn::BPF_W,
                        10,
                        src_reg,
                        off,
                        0,
                    ));
                }
                0x04 => {
                    let op = (code & 0xf0) as u8;
                    if src {
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_ALU64 | op | bpf_insn::BPF_X,
                            0,
                            6,
                            0,
                            0,
                        ));
                    } else {
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_ALU64 | op | bpf_insn::BPF_K,
                            0,
                            0,
                            0,
                            i.k as i32,
                        ));
                    }
                }
                0x05 => {
                    let op = (code & 0xf0) as u8;
                    let jt = i.jt as u32;
                    let jf = i.jf as u32;
                    if op == 0x00 {
                        let target = epc_map[(pc as u32 + 1 + jt) as usize] as isize;
                        let off = target - epc as isize - 1;
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_JMP | bpf_insn::BPF_JA,
                            0,
                            0,
                            off as i16,
                            0,
                        ));
                    } else {
                        let t_true = epc_map[(pc as u32 + 1 + jt) as usize] as isize;
                        let t_false = epc_map[(pc as u32 + 1 + jf) as usize] as isize;
                        let off_true = t_true - epc as isize - 1;
                        let off_false = t_false - epc as isize - 2;
                        if src {
                            out.push(BpfInsn::new(
                                bpf_insn::BPF_JMP | op | bpf_insn::BPF_X,
                                0,
                                6,
                                off_true as i16,
                                0,
                            ));
                        } else {
                            out.push(BpfInsn::new(
                                bpf_insn::BPF_JMP | op | bpf_insn::BPF_K,
                                0,
                                0,
                                off_true as i16,
                                i.k as i32,
                            ));
                        }
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_JMP | bpf_insn::BPF_JA,
                            0,
                            0,
                            off_false as i16,
                            0,
                        ));
                    }
                }
                0x06 => {
                    if src {
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_JMP | bpf_insn::BPF_EXIT,
                            0,
                            0,
                            0,
                            0,
                        ));
                    } else {
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_ALU64 | bpf_insn::BPF_MOV | bpf_insn::BPF_K,
                            0,
                            0,
                            0,
                            i.k as i32,
                        ));
                        out.push(BpfInsn::new(
                            bpf_insn::BPF_JMP | bpf_insn::BPF_EXIT,
                            0,
                            0,
                            0,
                            0,
                        ));
                    }
                }
                _ => {
                    out.push(BpfInsn::new(
                        bpf_insn::BPF_ALU64 | bpf_insn::BPF_MOV | bpf_insn::BPF_K,
                        0,
                        0,
                        0,
                        0,
                    ));
                }
            }
        }

        debug_assert_eq!(out.len(), total, "eBPF emission count mismatch");
        out
    }
}

/// Assign a precedence rank to a seccomp return action.
/// Lower rank = higher priority.  Linux precedence:
///   KILL_PROCESS (0) < KILL_THREAD (1) < TRAP (2) < ERRNO (3)
///   < TRACE (4) < LOG (5) < ALLOW (6)
#[allow(dead_code)]
fn action_precedence(action: u32) -> u8 {
    match action & seccomp_ret::ACTION_FULL {
        seccomp_ret::KILL_PROCESS => 0,
        seccomp_ret::KILL_THREAD => 1,
        seccomp_ret::TRAP => 2,
        seccomp_ret::ERRNO => 3,
        seccomp_ret::TRACE => 4,
        seccomp_ret::LOG => 5,
        _ => 6,
    }
}

// ----------------------------------------------------------------
// sys_seccomp
// ----------------------------------------------------------------

/// `seccomp(2)` syscall — operate on the Secure Computing state of the
/// calling thread.
///
/// Parameters:
/// - `op`: operation (SET_MODE_STRICT, SET_MODE_FILTER,
///   GET_ACTION_AVAIL, GET_NOTIF_SIZES)
/// - `flags`: filter flags (only meaningful for SET_MODE_FILTER)
/// - `args`: operation-specific pointer (e.g. `sock_fprog*` for FILTER)
///
/// Errors: EINVAL, EACCES, EFAULT.
pub fn sys_seccomp(op: u32, flags: u32, args: *const ()) -> AxResult<isize> {
    use ax_task::current;
    use seccomp_mode::{DISABLED, FILTER, STRICT};
    use starry_vm::VmPtr;

    use crate::task::get_task;

    match op {
        seccomp_op::SET_MODE_STRICT => {
            if flags != 0 || !args.is_null() {
                return Err(AxError::InvalidInput);
            }
            let curr = current();
            let thr = curr.as_thread();
            if thr.seccomp_mode() != DISABLED {
                return Err(AxError::InvalidInput);
            }
            thr.set_seccomp_mode(STRICT);
            Ok(0)
        }
        seccomp_op::SET_MODE_FILTER => {
            let filter_flags = SeccompFilterFlags::from_bits(flags).ok_or(AxError::InvalidInput)?;
            let curr = current();
            let thr = curr.as_thread();

            if !thr.no_new_privs() && thr.cred().euid != 0 {
                return Err(AxError::PermissionDenied);
            }
            if thr.seccomp_mode() == STRICT {
                return Err(AxError::InvalidInput);
            }
            if args.is_null() {
                return Err(AxError::BadAddress);
            }
            let prog: SockFprog = (args as *const SockFprog).vm_read()?;
            if prog.len == 0 || prog.len > 4096 {
                return Err(AxError::InvalidInput);
            }
            let filter_ptr = prog.filter_ptr as *const SockFilter;
            if filter_ptr.is_null() {
                return Err(AxError::BadAddress);
            }

            let mut insns: Vec<SeccompInsn> = Vec::with_capacity(prog.len as usize);
            for i in 0..prog.len as usize {
                let sf: SockFilter = filter_ptr.wrapping_add(i).vm_read()?;
                insns.push(sf);
            }

            let filter_data = SeccompFilterData { flags, insns };

            // TSYNC: propagate to all threads in the process
            if filter_flags.contains(SeccompFilterFlags::TSYNC) {
                let tg = &thr.proc_data.proc;
                for tid in tg.threads().iter() {
                    if let Ok(other) = get_task(*tid) {
                        let other_thr = other.as_thread();
                        if other_thr.seccomp_mode() == DISABLED
                            || other_thr.seccomp_mode() == FILTER
                        {
                            other_thr.set_seccomp_mode(FILTER);
                            other_thr.add_seccomp_filter(flags, filter_data.insns.clone());
                        }
                    }
                }
            }

            thr.set_seccomp_mode(FILTER);
            thr.add_seccomp_filter(flags, filter_data.insns);

            Ok(0)
        }
        seccomp_op::GET_ACTION_AVAIL => {
            if flags != 0 {
                return Err(AxError::InvalidInput);
            }
            if args.is_null() {
                return Err(AxError::BadAddress);
            }
            let action: u32 = (args as *const u32).vm_read()?;
            let avail: u32 = match action & seccomp_ret::ACTION_FULL {
                seccomp_ret::KILL_PROCESS
                | seccomp_ret::KILL_THREAD
                | seccomp_ret::TRAP
                | seccomp_ret::ERRNO
                | seccomp_ret::TRACE
                | seccomp_ret::LOG
                | seccomp_ret::ALLOW => 1,
                _ => 0,
            };
            (args as *mut u32).vm_write(avail)?;
            Ok(0)
        }
        seccomp_op::GET_NOTIF_SIZES => {
            if flags != 0 {
                return Err(AxError::InvalidInput);
            }
            if args.is_null() {
                return Err(AxError::BadAddress);
            }
            let sizes = SeccompNotifSizes {
                seccomp_notif: 0,
                seccomp_notif_resp: 0,
                seccomp_data: 0,
            };
            (args as *mut SeccompNotifSizes).vm_write(sizes)?;
            Ok(0)
        }
        _ => Err(AxError::InvalidInput),
    }
}

// ----------------------------------------------------------------
// check_seccomp_syscall – enforcement called before every syscall
// ----------------------------------------------------------------

/// Check seccomp enforcement before dispatching a syscall.
///
/// Called from the syscall dispatch loop.  In FILTER mode with the
/// `ebpf` feature enabled, the stored cBPF instructions are converted
/// to eBPF at runtime and executed through the eBPF VM.
///
/// Returns `true` if the syscall should proceed, `false` if seccomp
/// blocked it (errno set or signal raised).
pub fn check_seccomp_syscall(
    sysno: Sysno,
    uctx: &mut ax_runtime::hal::cpu::uspace::UserContext,
) -> bool {
    use ax_task::current;
    use seccomp_mode::{DISABLED, FILTER, STRICT};

    use crate::task::AsThread;

    let curr = current();
    let thr = curr.as_thread();
    match thr.seccomp_mode() {
        DISABLED => true,
        STRICT => {
            let allowed = matches!(
                sysno,
                Sysno::read
                    | Sysno::write
                    | Sysno::exit
                    | Sysno::exit_group
                    | Sysno::rt_sigreturn
                    | Sysno::readv
                    | Sysno::writev
                    | Sysno::seccomp
            );
            if !allowed {
                warn!("seccomp STRICT: killing thread for forbidden syscall {sysno:?}");
                use starry_signal::{SignalInfo, Signo};

                use crate::task::raise_signal_fatal;
                let _ = raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSYS), uctx);
                return false;
            }
            true
        }
        #[cfg(feature = "ebpf")]
        FILTER => {
            let filters = thr.seccomp_filters();
            if filters.is_empty() {
                return true;
            }

            let data = SeccompData::from_uctx(sysno, uctx);

            let (action, data_val) = {
                let data_ptr = &data as *const SeccompData as u64;
                let ctx_size = core::mem::size_of::<SeccompData>() as u32;
                let mut best_action = seccomp_ret::ALLOW;
                let mut best_data = 0u32;
                let mut best_rank = action_precedence(seccomp_ret::ALLOW);

                for f in &filters {
                    let ebpf = seccomp_ebpf_convert::cbpf_to_ebpf(&f.insns);
                    let ret = crate::ebpf::run_seccomp_bpf(&ebpf, data_ptr, ctx_size);
                    let rank = action_precedence(ret);
                    if rank < best_rank {
                        best_rank = rank;
                        best_action = ret & seccomp_ret::ACTION_FULL;
                        best_data = ret & seccomp_ret::DATA;
                    }
                }
                (best_action, best_data)
            };

            match action {
                seccomp_ret::ALLOW => true,
                seccomp_ret::LOG => {
                    debug!("seccomp LOG: syscall {sysno:?}");
                    true
                }
                seccomp_ret::ERRNO => {
                    let errno = (data_val as i32).max(1) as usize;
                    uctx.set_retval(-(errno as isize) as usize);
                    false
                }
                seccomp_ret::TRAP => {
                    warn!("seccomp TRAP: killing thread for {sysno:?} (data=0x{data_val:x})");
                    use starry_signal::{SignalInfo, Signo};

                    use crate::task::raise_signal_fatal;
                    let _ = raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSYS), uctx);
                    false
                }
                seccomp_ret::TRACE => {
                    warn!("seccomp TRACE: no tracer, killing thread for {sysno:?}");
                    use starry_signal::{SignalInfo, Signo};

                    use crate::task::raise_signal_fatal;
                    let _ = raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSYS), uctx);
                    false
                }
                seccomp_ret::KILL_THREAD => {
                    warn!("seccomp KILL_THREAD: killing thread for {sysno:?}");
                    use starry_signal::{SignalInfo, Signo};

                    use crate::task::raise_signal_fatal;
                    let _ = raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSYS), uctx);
                    false
                }
                seccomp_ret::KILL_PROCESS => {
                    warn!("seccomp KILL_PROCESS: killing process for {sysno:?}");
                    use starry_signal::{SignalInfo, Signo};

                    use crate::task::raise_signal_fatal;
                    let _ = raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSYS), uctx);
                    false
                }
                _ => {
                    warn!("seccomp: unknown action 0x{action:x}, killing thread for {sysno:?}");
                    use starry_signal::{SignalInfo, Signo};

                    use crate::task::raise_signal_fatal;
                    let _ = raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSYS), uctx);
                    false
                }
            }
        }
        #[cfg(not(feature = "ebpf"))]
        FILTER => {
            warn!("seccomp FILTER: ebpf feature required, allowing {sysno:?}");
            true
        }
        _ => true,
    }
}

#[cfg(target_arch = "riscv64")]
pub fn sys_riscv_flush_icache() -> AxResult<isize> {
    riscv::asm::fence_i();
    Ok(0)
}

#[cfg(target_arch = "riscv64")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct RiscvHwprobe {
    key: i64,
    value: u64,
}

#[cfg(target_arch = "riscv64")]
const RISCV_HWPROBE_KEY_BASE_BEHAVIOR: i64 = 3;
#[cfg(target_arch = "riscv64")]
const RISCV_HWPROBE_BASE_BEHAVIOR_IMA: u64 = 1 << 0;
#[cfg(target_arch = "riscv64")]
const RISCV_HWPROBE_KEY_IMA_EXT_0: i64 = 4;
#[cfg(target_arch = "riscv64")]
const RISCV_HWPROBE_IMA_FD: u64 = 1 << 0;
#[cfg(target_arch = "riscv64")]
const RISCV_HWPROBE_IMA_C: u64 = 1 << 1;
#[cfg(target_arch = "riscv64")]
const RISCV_HWPROBE_KEY_CPUPERF_0: i64 = 5;
#[cfg(target_arch = "riscv64")]
const RISCV_HWPROBE_KEY_MISALIGNED_SCALAR_PERF: i64 = 9;
#[cfg(target_arch = "riscv64")]
const RISCV_HWPROBE_KEY_MISALIGNED_VECTOR_PERF: i64 = 10;

#[cfg(target_arch = "riscv64")]
pub fn sys_riscv_hwprobe(
    pairs: *mut u8,
    pair_count: usize,
    cpu_count: usize,
    cpus: *const usize,
    flags: u32,
) -> AxResult<isize> {
    if flags != 0 || cpu_count != 0 || !cpus.is_null() {
        return Err(AxError::InvalidInput);
    }
    if pair_count == 0 {
        return Ok(0);
    }
    if pair_count > isize::MAX as usize / core::mem::size_of::<RiscvHwprobe>() {
        return Err(AxError::InvalidInput);
    }

    let pairs = UserPtr::<RiscvHwprobe>::from(pairs.cast()).get_as_mut_slice(pair_count)?;
    for pair in pairs {
        match pair.key {
            RISCV_HWPROBE_KEY_BASE_BEHAVIOR => pair.value = RISCV_HWPROBE_BASE_BEHAVIOR_IMA,
            RISCV_HWPROBE_KEY_IMA_EXT_0 => {
                pair.value = RISCV_HWPROBE_IMA_FD | RISCV_HWPROBE_IMA_C;
            }
            RISCV_HWPROBE_KEY_CPUPERF_0
            | RISCV_HWPROBE_KEY_MISALIGNED_SCALAR_PERF
            | RISCV_HWPROBE_KEY_MISALIGNED_VECTOR_PERF => {
                pair.value = 0;
            }
            _ => {
                pair.key = -1;
                pair.value = 0;
            }
        }
    }

    Ok(0)
}
