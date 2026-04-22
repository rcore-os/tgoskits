use alloc::{sync::Arc, vec, vec::Vec};
use core::{ffi::c_char, mem::MaybeUninit};

use ax_config::ARCH;
use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs::FS_CONTEXT;
use ax_hal::{mem::total_ram_size, time::monotonic_time};
use ax_task::current;
use linux_raw_sys::{
    general::{GRND_INSECURE, GRND_NONBLOCK, GRND_RANDOM},
    system::{new_utsname, sysinfo},
};
use starry_vm::{VmMutPtr, vm_read_slice, vm_write_slice};

use crate::task::{AsThread, processes};

/// Sentinel value meaning "don't change this ID" (userspace passes -1 as signed,
/// which becomes `u32::MAX` after the `as u32` cast in the dispatch table).
const NOCHG: u32 = u32::MAX;

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
    thread.set_cred(new);
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
    thread.set_cred(new);
    Ok(0)
}

// ── setuid / setgid ─────────────────────────────────────────────────

pub fn sys_setuid(uid: u32) -> AxResult<isize> {
    debug!("sys_setuid <= uid: {uid}");
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
    thread.set_cred(new);
    Ok(0)
}

pub fn sys_setgid(gid: u32) -> AxResult<isize> {
    debug!("sys_setgid <= gid: {gid}");
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
    thread.set_cred(new);
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
    thread.set_cred(new);
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
    thread.set_cred(new);
    Ok(0)
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
    // FIXME: Zeroable
    let mut kinfo: sysinfo = unsafe { core::mem::zeroed() };
    kinfo.uptime = monotonic_time().as_secs() as _;
    kinfo.totalram = total_ram_size() as _;
    kinfo.procs = processes().len() as _;
    kinfo.mem_unit = 1;
    info.vm_write(kinfo)?;
    Ok(0)
}

pub fn sys_syslog(type_: i32, buf: *mut c_char, len: usize) -> AxResult<isize> {
    const SYSLOG_ACTION_READ: i32 = 2;
    const SYSLOG_ACTION_READ_ALL: i32 = 3;
    const SYSLOG_ACTION_READ_CLEAR: i32 = 4;
    const SYSLOG_ACTION_CONSOLE_LEVEL: i32 = 8;
    const SYSLOG_ACTION_SIZE_BUFFER: i32 = 10;
    const KLOG_BUF_SIZE: isize = 16384;

    match type_ {
        // CLOSE(0), OPEN(1): NOP
        0 | 1 => Ok(0),

        // READ(2), READ_ALL(3), READ_CLEAR(4): need buf != NULL and len >= 0
        SYSLOG_ACTION_READ | SYSLOG_ACTION_READ_ALL | SYSLOG_ACTION_READ_CLEAR => {
            if buf.is_null() {
                return Err(AxError::from(LinuxError::EINVAL));
            }
            if (len as isize) < 0 {
                return Err(AxError::from(LinuxError::EINVAL));
            }
            Ok(0)
        }

        // CLEAR(5): NOP
        5 => Ok(0),

        // CONSOLE_OFF(6), CONSOLE_ON(7): NOP
        6 | 7 => Ok(0),

        // CONSOLE_LEVEL(8): level must be 1..=8
        SYSLOG_ACTION_CONSOLE_LEVEL => {
            let level = len as i32;
            if level < 1 || level > 8 {
                return Err(AxError::from(LinuxError::EINVAL));
            }
            Ok(0)
        }

        // SIZE_UNREAD(9): no unread data
        9 => Ok(0),

        // SIZE_BUFFER(10): return total buffer size
        SYSLOG_ACTION_SIZE_BUFFER => Ok(KLOG_BUF_SIZE),

        // Any other type: invalid
        _ => Err(AxError::from(LinuxError::EINVAL)),
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
    let flags = GetRandomFlags::from_bits_retain(flags);

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

pub fn sys_seccomp(_op: u32, _flags: u32, _args: *const ()) -> AxResult<isize> {
    warn!("dummy sys_seccomp");
    Ok(0)
}

#[cfg(target_arch = "riscv64")]
pub fn sys_riscv_flush_icache() -> AxResult<isize> {
    riscv::asm::fence_i();
    Ok(0)
}
