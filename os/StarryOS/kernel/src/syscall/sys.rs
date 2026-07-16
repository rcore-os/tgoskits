use alloc::{sync::Arc, vec, vec::Vec};
use core::{
    ffi::c_char,
    mem::{MaybeUninit, offset_of},
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs_ng::vfs::current_fs_context;
use ax_sync::PiMutex;
use linux_raw_sys::{
    general::{GRND_INSECURE, GRND_NONBLOCK, GRND_RANDOM},
    system::{new_utsname, sysinfo},
};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer},
};
use starry_vm::{VmMutPtr, VmPtr, vm_read_slice, vm_write_slice};

use crate::{
    mm::UserPtr,
    task::{SockFilter, SockFprog, current_user_task, get_task, processes},
};

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
const SECCOMP_SET_MODE_STRICT: u32 = 0;
const SECCOMP_SET_MODE_FILTER: u32 = 1;
const SECCOMP_GET_ACTION_AVAIL: u32 = 2;
const SECCOMP_FILTER_FLAG_TSYNC: u32 = 1 << 0;
const SECCOMP_FILTER_FLAG_LOG: u32 = 1 << 1;
const SECCOMP_FILTER_FLAG_SPEC_ALLOW: u32 = 1 << 2;
const SECCOMP_FILTER_FLAG_TSYNC_ESRCH: u32 = 1 << 4;
const SECCOMP_ALLOWED_FLAGS: u32 = SECCOMP_FILTER_FLAG_TSYNC
    | SECCOMP_FILTER_FLAG_LOG
    | SECCOMP_FILTER_FLAG_SPEC_ALLOW
    | SECCOMP_FILTER_FLAG_TSYNC_ESRCH;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_KILL_THREAD: u32 = 0x0000_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_LOG: u32 = 0x7ffc_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const LINUX_REBOOT_MAGIC1: u32 = 0xfee1_dead;
const LINUX_REBOOT_MAGIC2: u32 = 0x2812_1969;
const LINUX_REBOOT_MAGIC2A: u32 = 0x0512_1996;
const LINUX_REBOOT_MAGIC2B: u32 = 0x1604_1998;
const LINUX_REBOOT_MAGIC2C: u32 = 0x2011_2000;
const LINUX_REBOOT_CMD_RESTART: u32 = 0x0123_4567;
const LINUX_REBOOT_CMD_RESTART2: u32 = 0xa1b2_c3d4;
const LINUX_REBOOT_CMD_CAD_ON: u32 = 0x89ab_cdef;
const LINUX_REBOOT_CMD_CAD_OFF: u32 = 0x0000_0000;
const LINUX_REBOOT_CMD_HALT: u32 = 0xcdef_0123;
const LINUX_REBOOT_CMD_POWER_OFF: u32 = 0x4321_fedc;

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

static SYSLOG_STATE: spin::LazyLock<PiMutex<SyslogState>> =
    spin::LazyLock::new(|| PiMutex::new(SyslogState::new()));

pub fn sys_reboot(magic: u32, magic2: u32, cmd: u32, _arg: usize) -> AxResult<isize> {
    if !current_user_task().as_thread().cred().has_cap_sys_boot() {
        return Err(AxError::from(LinuxError::EPERM));
    }

    if magic != LINUX_REBOOT_MAGIC1
        || !matches!(
            magic2,
            LINUX_REBOOT_MAGIC2
                | LINUX_REBOOT_MAGIC2A
                | LINUX_REBOOT_MAGIC2B
                | LINUX_REBOOT_MAGIC2C
        )
    {
        return Err(AxError::from(LinuxError::EINVAL));
    }

    match cmd {
        LINUX_REBOOT_CMD_CAD_ON | LINUX_REBOOT_CMD_CAD_OFF => Ok(0),
        LINUX_REBOOT_CMD_RESTART | LINUX_REBOOT_CMD_RESTART2 => {
            let _ = ax_fs_ng::shutdown_filesystems();
            ax_runtime::hal::power::system_reset()
        }
        LINUX_REBOOT_CMD_HALT | LINUX_REBOOT_CMD_POWER_OFF => {
            let _ = ax_fs_ng::shutdown_filesystems();
            ax_runtime::hal::power::system_off()
        }
        _ => Err(AxError::from(LinuxError::EINVAL)),
    }
}

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

fn commit_cred_with_id_rules(
    thread: &crate::task::Thread,
    old: &crate::task::Cred,
    mut new: crate::task::Cred,
) {
    new.apply_id_change_capability_rules(old);
    thread.set_cred(new);
}

fn user_ns_overflow_uid() -> u32 {
    let curr = current_user_task();
    let nsproxy = curr.as_thread().proc_data.nsproxy.lock();
    let ns = nsproxy.user_ns.lock();
    if ns.is_root || ns.uid_mapped {
        return 0;
    }
    65534
}

fn user_ns_overflow_gid() -> u32 {
    let curr = current_user_task();
    let nsproxy = curr.as_thread().proc_data.nsproxy.lock();
    let ns = nsproxy.user_ns.lock();
    if ns.is_root || ns.gid_mapped {
        return 0;
    }
    65534
}

pub fn sys_getuid() -> AxResult<isize> {
    let overflow = user_ns_overflow_uid();
    if overflow != 0 {
        return Ok(overflow as isize);
    }
    let cred = current_user_task().as_thread().cred();
    Ok(cred.uid as isize)
}

pub fn sys_geteuid() -> AxResult<isize> {
    let overflow = user_ns_overflow_uid();
    if overflow != 0 {
        return Ok(overflow as isize);
    }
    let cred = current_user_task().as_thread().cred();
    Ok(cred.euid as isize)
}

pub fn sys_getgid() -> AxResult<isize> {
    let overflow = user_ns_overflow_gid();
    if overflow != 0 {
        return Ok(overflow as isize);
    }
    let cred = current_user_task().as_thread().cred();
    Ok(cred.gid as isize)
}

pub fn sys_getegid() -> AxResult<isize> {
    let overflow = user_ns_overflow_gid();
    if overflow != 0 {
        return Ok(overflow as isize);
    }
    let cred = current_user_task().as_thread().cred();
    Ok(cred.egid as isize)
}

pub fn sys_getresuid(ruid: *mut u32, euid: *mut u32, suid: *mut u32) -> AxResult<isize> {
    let overflow = user_ns_overflow_uid();
    if overflow != 0 {
        ruid.vm_write(overflow)?;
        euid.vm_write(overflow)?;
        suid.vm_write(overflow)?;
        return Ok(0);
    }
    let cred = current_user_task().as_thread().cred();
    ruid.vm_write(cred.uid)?;
    euid.vm_write(cred.euid)?;
    suid.vm_write(cred.suid)?;
    Ok(0)
}

pub fn sys_getresgid(rgid: *mut u32, egid: *mut u32, sgid: *mut u32) -> AxResult<isize> {
    let overflow = user_ns_overflow_gid();
    if overflow != 0 {
        rgid.vm_write(overflow)?;
        egid.vm_write(overflow)?;
        sgid.vm_write(overflow)?;
        return Ok(0);
    }
    let cred = current_user_task().as_thread().cred();
    rgid.vm_write(cred.gid)?;
    egid.vm_write(cred.egid)?;
    sgid.vm_write(cred.sgid)?;
    Ok(0)
}

// ── setresuid / setresgid ────────────────────────────────────────────

pub fn sys_setresuid(ruid: u32, euid: u32, suid: u32) -> AxResult<isize> {
    debug!("sys_setresuid <= ruid: {ruid}, euid: {euid}, suid: {suid}");
    let thread = current_user_task();
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
    commit_cred_with_id_rules(thread, &old, new);
    if reset_dumpable {
        thread.proc_data.set_dumpable(0);
    }
    Ok(0)
}

pub fn sys_setresgid(rgid: u32, egid: u32, sgid: u32) -> AxResult<isize> {
    debug!("sys_setresgid <= rgid: {rgid}, egid: {egid}, sgid: {sgid}");
    let thread = current_user_task();
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
    commit_cred_with_id_rules(thread, &old, new);
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
    let thread = current_user_task();
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
    commit_cred_with_id_rules(thread, &old, new);
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
    let thread = current_user_task();
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
    commit_cred_with_id_rules(thread, &old, new);
    if reset_dumpable {
        thread.proc_data.set_dumpable(0);
    }
    Ok(0)
}

// ── setreuid / setregid ─────────────────────────────────────────────

pub fn sys_setreuid(ruid: u32, euid: u32) -> AxResult<isize> {
    debug!("sys_setreuid <= ruid: {ruid}, euid: {euid}");
    let thread = current_user_task();
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
    commit_cred_with_id_rules(thread, &old, new);
    if reset_dumpable {
        thread.proc_data.set_dumpable(0);
    }
    Ok(0)
}

pub fn sys_setregid(rgid: u32, egid: u32) -> AxResult<isize> {
    debug!("sys_setregid <= rgid: {rgid}, egid: {egid}");
    let thread = current_user_task();
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
    commit_cred_with_id_rules(thread, &old, new);
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
    let thread = current_user_task();
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
        commit_cred_with_id_rules(thread, &old, new);
        if reset_dumpable {
            thread.proc_data.set_dumpable(0);
        }
    }
    // Always return previous fsuid, even when the request was ignored.
    Ok(prev_fsuid as isize)
}

pub fn sys_setfsgid(fsgid: u32) -> AxResult<isize> {
    debug!("sys_setfsgid <= fsgid: {fsgid}");
    let thread = current_user_task();
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
        commit_cred_with_id_rules(thread, &old, new);
        if reset_dumpable {
            thread.proc_data.set_dumpable(0);
        }
    }
    Ok(prev_fsgid as isize)
}

pub fn sys_getgroups(size: usize, list: *mut u32) -> AxResult<isize> {
    debug!("sys_getgroups <= size: {size}");
    let cred = current_user_task().as_thread().cred();
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
    let thread = current_user_task();
    let thread = thread.as_thread();
    let old = thread.cred();

    if !old.has_cap_setgid() {
        return Err(AxError::OperationNotPermitted);
    }
    // Linux 3.19+: writing "deny" to /proc/self/setgroups prevents setgroups(2).
    if thread.setgroups_deny() {
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
    commit_cred_with_id_rules(thread, &old, new);
    Ok(0)
}

pub fn sys_uname(name: *mut new_utsname) -> AxResult<isize> {
    let curr = current_user_task();
    // Build the utsname inside a block so the SpinNoIrq guard is dropped
    // before we touch user memory via vm_write (access_user_memory requires
    // IRQs enabled, but SpinNoIrq disables them).
    let uts = {
        let nsproxy = curr.as_thread().proc_data.nsproxy.lock();
        let ns = nsproxy.uts_ns.lock();
        axnsproxy::build_utsname(&ns)
    };
    write_utsname(name, uts)?;
    Ok(0)
}

fn write_utsname(user: *mut new_utsname, value: new_utsname) -> AxResult<()> {
    let user = UserPtr::from(user);
    user.write_field_slice(offset_of!(new_utsname, sysname), &value.sysname)?;
    user.write_field_slice(offset_of!(new_utsname, nodename), &value.nodename)?;
    user.write_field_slice(offset_of!(new_utsname, release), &value.release)?;
    user.write_field_slice(offset_of!(new_utsname, version), &value.version)?;
    user.write_field_slice(offset_of!(new_utsname, machine), &value.machine)?;
    user.write_field_slice(offset_of!(new_utsname, domainname), &value.domainname)
}

pub fn sys_sethostname(name: *const c_char, len: usize) -> AxResult<isize> {
    if len > 64 {
        return Err(AxError::InvalidInput);
    }
    let curr = current_user_task();
    if curr.as_thread().cred().euid != 0 {
        return Err(AxError::OperationNotPermitted);
    }
    let mut buf: Vec<MaybeUninit<u8>> = vec![MaybeUninit::uninit(); len];
    vm_read_slice(name.cast::<u8>(), &mut buf)?;
    let bytes: Vec<u8> = unsafe { buf.into_iter().map(|v| v.assume_init()).collect() };
    let mut nodename: [c_char; 65] = [0; 65];
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), nodename.as_mut_ptr(), len);
    }
    let proc_data = &curr.as_thread().proc_data;
    proc_data.nsproxy.lock().uts_ns.lock().nodename = nodename;
    Ok(0)
}

pub fn sys_setdomainname(name: *const c_char, len: usize) -> AxResult<isize> {
    if len > 64 {
        return Err(AxError::InvalidInput);
    }
    let curr = current_user_task();
    if curr.as_thread().cred().euid != 0 {
        return Err(AxError::OperationNotPermitted);
    }
    let mut buf: Vec<MaybeUninit<u8>> = vec![MaybeUninit::uninit(); len];
    vm_read_slice(name.cast::<u8>(), &mut buf)?;
    let bytes: Vec<u8> = unsafe { buf.into_iter().map(|v| v.assume_init()).collect() };
    let mut domainname: [c_char; 65] = [0; 65];
    unsafe {
        core::ptr::copy_nonoverlapping(
            bytes.as_ptr().cast::<c_char>(),
            domainname.as_mut_ptr(),
            len,
        );
    }
    let proc_data = &curr.as_thread().proc_data;
    proc_data.nsproxy.lock().uts_ns.lock().domainname = domainname;
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

    write_sysinfo(info, kinfo)?;
    Ok(0)
}

fn write_sysinfo(user: *mut sysinfo, value: sysinfo) -> AxResult<()> {
    let user = UserPtr::from(user);
    user.write_field(offset_of!(sysinfo, uptime), value.uptime)?;
    user.write_field(offset_of!(sysinfo, loads), value.loads)?;
    user.write_field(offset_of!(sysinfo, totalram), value.totalram)?;
    user.write_field(offset_of!(sysinfo, freeram), value.freeram)?;
    user.write_field(offset_of!(sysinfo, sharedram), value.sharedram)?;
    user.write_field(offset_of!(sysinfo, bufferram), value.bufferram)?;
    user.write_field(offset_of!(sysinfo, totalswap), value.totalswap)?;
    user.write_field(offset_of!(sysinfo, freeswap), value.freeswap)?;
    user.write_field(offset_of!(sysinfo, procs), value.procs)?;
    user.write_field(offset_of!(sysinfo, pad), value.pad)?;
    user.write_field(offset_of!(sysinfo, totalhigh), value.totalhigh)?;
    user.write_field(offset_of!(sysinfo, freehigh), value.freehigh)?;
    user.write_field(offset_of!(sysinfo, mem_unit), value.mem_unit)
}

fn require_syslog_privilege() -> AxResult<()> {
    if current_user_task().as_thread().cred().euid == 0 {
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

    let f = current_fs_context().lock().resolve(path)?;
    let mut kbuf = vec![0; len];
    let len = f.entry().as_file()?.read_at(&mut kbuf, 0)?;

    vm_write_slice(buf, &kbuf)?;

    Ok(len as _)
}

fn check_seccomp_install_permission() -> AxResult<()> {
    let curr = current_user_task();
    let thread = curr.as_thread();
    if thread.no_new_privs() || thread.cred().has_cap_sys_admin() {
        Ok(())
    } else {
        Err(AxError::OperationNotPermitted)
    }
}

fn read_seccomp_filter(args: *const ()) -> AxResult<Vec<SockFilter>> {
    if args.is_null() {
        return Err(AxError::BadAddress);
    }
    let prog = unsafe { (args as *const SockFprog).vm_read_uninit()?.assume_init() };
    if prog.len == 0 || prog.filter.is_null() {
        return Err(AxError::InvalidInput);
    }
    let mut raw = vec![MaybeUninit::<SockFilter>::uninit(); prog.len as usize];
    vm_read_slice(prog.filter, &mut raw)?;
    Ok(raw
        .into_iter()
        .map(|insn| unsafe { insn.assume_init() })
        .collect())
}

fn seccomp_action_available(args: *const ()) -> AxResult<isize> {
    if args.is_null() {
        return Err(AxError::BadAddress);
    }
    let action = unsafe { (args as *const u32).vm_read_uninit()?.assume_init() };
    match action {
        SECCOMP_RET_ALLOW
        | SECCOMP_RET_LOG
        | SECCOMP_RET_ERRNO
        | SECCOMP_RET_KILL_THREAD
        | SECCOMP_RET_KILL_PROCESS => Ok(0),
        _ => Err(AxError::OperationNotSupported),
    }
}

fn sync_seccomp_to_thread_group() {
    let curr = current_user_task();
    let thread = curr.as_thread();
    let state = thread.seccomp_state();
    for tid in thread.proc_data.proc.threads() {
        if tid == thread.tid() {
            continue;
        }
        if let Ok(task) = get_task(tid) {
            task.as_thread().set_seccomp_state(state.clone());
        }
    }
}

pub fn sys_seccomp(op: u32, flags: u32, args: *const ()) -> AxResult<isize> {
    if flags & !SECCOMP_ALLOWED_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }

    match op {
        SECCOMP_SET_MODE_STRICT => {
            if flags != 0 || !args.is_null() {
                return Err(AxError::InvalidInput);
            }
            current_user_task().as_thread().install_seccomp_strict()?;
        }
        SECCOMP_SET_MODE_FILTER => {
            check_seccomp_install_permission()?;
            let filter = read_seccomp_filter(args)?;
            let curr = current_user_task();
            let thread = curr.as_thread();
            thread.append_seccomp_filter(filter)?;
            if flags & SECCOMP_FILTER_FLAG_TSYNC != 0 {
                sync_seccomp_to_thread_group();
            }
        }
        SECCOMP_GET_ACTION_AVAIL => return seccomp_action_available(args),
        _ => return Err(AxError::InvalidInput),
    }

    Ok(0)
}

#[cfg(target_arch = "riscv64")]
const SYS_RISCV_FLUSH_ICACHE_LOCAL: usize = 1;

#[cfg(target_arch = "riscv64")]
pub fn sys_riscv_flush_icache(start: usize, end: usize, flags: usize) -> AxResult<isize> {
    if flags & !SYS_RISCV_FLUSH_ICACHE_LOCAL != 0 {
        return Err(AxError::InvalidInput);
    }
    if end < start {
        return Err(AxError::InvalidInput);
    }

    if flags & SYS_RISCV_FLUSH_ICACHE_LOCAL != 0 {
        ax_runtime::hal::cache::flush_icache_all();
    } else {
        ax_runtime::hal::cache::flush_icache_all_cpus();
    }
    Ok(0)
}

#[cfg(target_arch = "riscv64")]
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
struct RiscvHwprobe {
    key: i64,
    value: u64,
}

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

    let input_pairs = crate::mm::UserConstPtr::<RiscvHwprobe>::from(pairs.cast_const().cast());
    let output_pairs = UserPtr::<RiscvHwprobe>::from(pairs.cast());
    let mut pairs = input_pairs.read_slice(pair_count)?;
    for pair in &mut pairs {
        if let Some(value) = ax_runtime::hal::cpu::cap::riscv_hwprobe(pair.key) {
            pair.value = value;
        } else {
            pair.key = -1;
            pair.value = 0;
        }
    }
    output_pairs.write_slice(&pairs)?;

    Ok(0)
}
