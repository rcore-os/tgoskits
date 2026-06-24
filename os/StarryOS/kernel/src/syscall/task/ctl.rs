//! Task-control syscalls: capabilities, `prctl`, personality, and NUMA policy.
//!
//! The capability helpers in this file implement the Linux `capget(2)` and
//! `capset(2)` ABI plus the capability-related `prctl(2)` operations.  They
//! translate between userspace's split `u32` capability arrays and StarryOS's
//! internal `Cred` bitmap fields.

use core::ffi::c_char;

use ax_errno::{AxError, AxResult};
use ax_task::current;
use linux_raw_sys::general::{__user_cap_data_struct, __user_cap_header_struct, CAP_LAST_CAP};
use starry_vm::{VmMutPtr, VmPtr, vm_write_slice};

use crate::{
    mm::vm_load_string,
    task::{AsThread, Cred, get_process_data, get_task},
};

const CAPABILITY_VERSION_3: u32 = 0x20080522;
const CAP_U32S_3: usize = 2;
const PERSONALITY_GET: u32 = 0xffff_ffff;
const PR_THP_DISABLE_EXCEPT_ADVISED: usize = 1 << 1;
const MPOL_DEFAULT: i32 = 0;
const MPOL_PREFERRED: i32 = 1;
const MPOL_BIND: i32 = 2;
const MPOL_INTERLEAVE: i32 = 3;
const MPOL_LOCAL: i32 = 4;
const MPOL_PREFERRED_MANY: i32 = 5;
const MPOL_WEIGHTED_INTERLEAVE: i32 = 6;
const MPOL_F_NODE: usize = 1 << 0;
const MPOL_F_ADDR: usize = 1 << 1;
const MPOL_F_MEMS_ALLOWED: usize = 1 << 2;
const MPOL_F_STATIC_NODES: i32 = 1 << 15;
const MPOL_F_RELATIVE_NODES: i32 = 1 << 14;
const MPOL_MODE_FLAGS: i32 = MPOL_F_STATIC_NODES | MPOL_F_RELATIVE_NODES;
const MPOL_MF_STRICT: u32 = 1 << 0;
const MPOL_MF_MOVE: u32 = 1 << 1;
const MPOL_MF_MOVE_ALL: u32 = 1 << 2;
const MPOL_MF_VALID: u32 = MPOL_MF_STRICT | MPOL_MF_MOVE | MPOL_MF_MOVE_ALL;

/// Split a NUMA policy mode from its optional mode flags.
fn parse_mempolicy_mode(mode: i32) -> AxResult<i32> {
    if mode < 0 {
        return Err(AxError::InvalidInput);
    }
    let policy = mode & !MPOL_MODE_FLAGS;
    match policy {
        MPOL_DEFAULT
        | MPOL_PREFERRED
        | MPOL_BIND
        | MPOL_INTERLEAVE
        | MPOL_LOCAL
        | MPOL_PREFERRED_MANY
        | MPOL_WEIGHTED_INTERLEAVE => Ok(policy),
        _ => Err(AxError::InvalidInput),
    }
}

/// Validate a user nodemask pointer when a policy consumes it.
fn check_nodemask(nodemask: *const usize, maxnode: usize) -> AxResult<()> {
    if !nodemask.is_null() && maxnode > 0 {
        nodemask.vm_read()?;
    }
    Ok(())
}

/// Validate the cap header and return the target pid (0 means self).
fn validate_cap_header(header_ptr: *mut __user_cap_header_struct) -> AxResult<u32> {
    // FIXME: AnyBitPattern
    let mut header = unsafe { header_ptr.vm_read_uninit()?.assume_init() };
    if header.version != CAPABILITY_VERSION_3 {
        header.version = CAPABILITY_VERSION_3;
        header_ptr.vm_write(header)?;
        return Err(AxError::InvalidInput);
    }
    let pid = header.pid as u32;
    let _ = get_process_data(pid)?;
    Ok(pid)
}

/// Read the credential set for the thread identified by TID (0 = self).
///
/// capget(2) operates on the thread identified by `header.pid`; on Linux
/// threads in the same thread group share the same `struct cred` by default,
/// so reading any thread's cred gives the same answer.
fn cred_for_pid(pid: u32) -> AxResult<alloc::sync::Arc<Cred>> {
    if pid == 0 {
        return Ok(current().as_thread().cred());
    }
    let task = get_task(pid).map_err(|_| AxError::NoSuchProcess)?;
    task.try_as_thread()
        .map(|t| t.cred())
        .ok_or(AxError::NoSuchProcess)
}

/// Validate a capability number and return its bit in the internal bitmap.
fn cap_bit(cap: u32) -> AxResult<u64> {
    if cap > CAP_LAST_CAP {
        return Err(AxError::InvalidInput);
    }
    Ok(1u64 << cap)
}

/// Merge the two u32 words from a Linux V3 capability array into one mask.
fn data_to_mask(
    data: &[__user_cap_data_struct; CAP_U32S_3],
    f: fn(&__user_cap_data_struct) -> u32,
) -> u64 {
    u64::from(f(&data[0])) | (u64::from(f(&data[1])) << 32)
}

/// Convert StarryOS credentials into Linux V3 userspace capability words.
fn cap_data_from_cred(cred: &Cred) -> [__user_cap_data_struct; CAP_U32S_3] {
    [
        __user_cap_data_struct {
            effective: cred.cap_effective as u32,
            permitted: cred.cap_permitted as u32,
            inheritable: cred.cap_inheritable as u32,
        },
        __user_cap_data_struct {
            effective: (cred.cap_effective >> 32) as u32,
            permitted: (cred.cap_permitted >> 32) as u32,
            inheritable: (cred.cap_inheritable >> 32) as u32,
        },
    ]
}

/// Implement `capget(2)`.
///
/// StarryOS supports the Linux V3 capability ABI.  When `data` is null, the
/// call only validates/fixes the header version as Linux does.  Otherwise, the
/// selected thread's effective, permitted, and inheritable sets are copied to
/// userspace.
pub fn sys_capget(
    header: *mut __user_cap_header_struct,
    data: *mut __user_cap_data_struct,
) -> AxResult<isize> {
    let pid = validate_cap_header(header)?;

    if data.is_null() {
        return Ok(0);
    }

    let cred = cred_for_pid(pid)?;
    let cap_data = cap_data_from_cred(&cred);
    unsafe {
        data.vm_write(cap_data[0])?;
        data.add(1).vm_write(cap_data[1])?;
    }
    Ok(0)
}

/// Implement `capset(2)` for the current thread.
///
/// The caller may only update its own credentials.  Effective capabilities must
/// remain a subset of permitted capabilities, permitted capabilities cannot be
/// expanded, and inheritable expansion follows Linux's `CAP_SETPCAP`/bounding
/// set rules.
pub fn sys_capset(
    header: *mut __user_cap_header_struct,
    data: *mut __user_cap_data_struct,
) -> AxResult<isize> {
    let pid = validate_cap_header(header)?;
    if data.is_null() {
        return Err(AxError::BadAddress);
    }

    let thread_ref = current();
    let thread = thread_ref.as_thread();
    if pid != 0 && pid != thread.tid() {
        return Err(AxError::OperationNotPermitted);
    }

    let requested = unsafe {
        [
            data.vm_read_uninit()?.assume_init(),
            data.add(1).vm_read_uninit()?.assume_init(),
        ]
    };
    let old = thread.cred();
    let cap_mask = Cred::cap_mask();
    let effective = data_to_mask(&requested, |d| d.effective) & cap_mask;
    let permitted = data_to_mask(&requested, |d| d.permitted) & cap_mask;
    let inheritable = data_to_mask(&requested, |d| d.inheritable) & cap_mask;

    if effective & !permitted != 0 {
        return Err(AxError::OperationNotPermitted);
    }

    let adds_permitted = permitted & !old.cap_permitted;
    let adds_inheritable = inheritable & !old.cap_inheritable;
    let may_expand = old.has_cap_setpcap();
    if adds_permitted != 0 {
        return Err(AxError::OperationNotPermitted);
    }
    if may_expand {
        if adds_inheritable & !old.cap_bounding != 0 {
            return Err(AxError::OperationNotPermitted);
        }
    } else if adds_inheritable & !(old.cap_inheritable | old.cap_permitted) != 0 {
        return Err(AxError::OperationNotPermitted);
    }

    let mut new = (*old).clone();
    new.cap_effective = effective;
    new.cap_permitted = permitted;
    new.cap_inheritable = inheritable;
    new.sanitize_capabilities();
    thread.set_cred(new);
    Ok(0)
}

pub fn sys_umask(mask: u32) -> AxResult<isize> {
    let curr = current();
    let old = curr.as_thread().proc_data.replace_umask(mask & 0o777);
    Ok(old as isize)
}

pub fn sys_personality(persona: usize) -> AxResult<isize> {
    let curr = current();
    let proc_data = &curr.as_thread().proc_data;
    let old = proc_data.personality();
    if persona as u32 != PERSONALITY_GET {
        proc_data.replace_personality(persona);
    }
    Ok(old as isize)
}

/// Get NUMA memory policy for a thread.
///
/// For single-node systems (which StarryOS currently models), all memory
/// is on node 0 with default policy MPOL_DEFAULT.
///
/// Arguments:
/// - policy: output pointer for policy mode (MPOL_DEFAULT, MPOL_BIND, etc.)
/// - nodemask: output pointer for node mask bitmap
/// - maxnode: size of nodemask bitmap in bits
/// - addr: memory address to query (when MPOL_F_ADDR flag set)
/// - flags: MPOL_F_NODE, MPOL_F_ADDR, MPOL_F_MEMS_ALLOWED
///
/// Returns 0 on success, or -errno on error.
pub fn sys_get_mempolicy(
    policy: *mut i32,
    nodemask: *mut usize,
    maxnode: usize,
    _addr: usize,
    flags: usize,
) -> AxResult<isize> {
    debug!(
        "sys_get_mempolicy <= policy: {:?}, nodemask: {:?}, maxnode: {}, flags: {:#x}",
        policy, nodemask, maxnode, flags
    );

    if flags & !(MPOL_F_NODE | MPOL_F_ADDR | MPOL_F_MEMS_ALLOWED) != 0 {
        return Err(AxError::InvalidInput);
    }
    if flags & MPOL_F_MEMS_ALLOWED != 0 && flags != MPOL_F_MEMS_ALLOWED {
        return Err(AxError::InvalidInput);
    }
    if flags & MPOL_F_NODE != 0 && flags & MPOL_F_ADDR == 0 {
        return Err(AxError::InvalidInput);
    }

    // StarryOS models one NUMA node, so every query resolves to node 0.
    if flags & MPOL_F_MEMS_ALLOWED != 0 {
        if !nodemask.is_null() && maxnode > 0 {
            nodemask.vm_write(1usize)?;
        }
        return Ok(0);
    }

    if flags & MPOL_F_NODE != 0 {
        if !policy.is_null() {
            policy.vm_write(0i32)?;
        }
        return Ok(0);
    }

    if !policy.is_null() {
        policy.vm_write(MPOL_DEFAULT)?;
    }

    if !nodemask.is_null() && maxnode > 0 {
        nodemask.vm_write(1usize)?;
    }

    Ok(0)
}

/// Set NUMA memory policy for a thread.
///
/// For single-node systems, this is a no-op that always succeeds.
///
/// Arguments:
/// - mode: policy mode (MPOL_DEFAULT, MPOL_BIND, MPOL_INTERLEAVE, etc.)
/// - nodemask: node mask bitmap
/// - maxnode: size of nodemask bitmap in bits
///
/// Returns 0 on success.
pub fn sys_set_mempolicy(mode: i32, nodemask: *const usize, maxnode: usize) -> AxResult<isize> {
    debug!("sys_set_mempolicy <= mode: {}", mode);

    let policy = parse_mempolicy_mode(mode)?;
    if policy != MPOL_DEFAULT {
        check_nodemask(nodemask, maxnode)?;
    }

    // Single-node system: accept valid policies and ignore placement.
    Ok(0)
}

/// Bind memory range to NUMA nodes.
///
/// For single-node systems, this is a no-op that always succeeds.
///
/// Arguments:
/// - addr: start address of memory range
/// - len: length of memory range
/// - mode: policy mode
/// - nodemask: node mask bitmap
/// - maxnode: size of nodemask bitmap in bits
/// - flags: MPOL_MF_STRICT, MPOL_MF_MOVE, MPOL_MF_MOVE_ALL
///
/// Returns 0 on success.
pub fn sys_mbind(
    addr: usize,
    len: usize,
    mode: i32,
    nodemask: *const usize,
    maxnode: usize,
    flags: u32,
) -> AxResult<isize> {
    debug!("sys_mbind <= mode: {}", mode);

    let policy = parse_mempolicy_mode(mode)?;
    if addr & 0xfff != 0 || len == 0 || flags & !MPOL_MF_VALID != 0 {
        return Err(AxError::InvalidInput);
    }
    if policy != MPOL_DEFAULT {
        check_nodemask(nodemask, maxnode)?;
    }

    // Single-node system: accept valid bindings and ignore placement.
    Ok(0)
}

/// prctl() is called with a first argument describing what to do, and further
/// arguments with a significance depending on the first one.
/// The first argument can be:
/// - PR_SET_NAME: set the name of the calling thread, using the value pointed to by `arg2`
/// - PR_GET_NAME: get the name of the calling
/// - PR_SET_SECCOMP: enable seccomp mode, with the mode specified in `arg2`
/// - PR_SET_CHILD_SUBREAPER / PR_GET_CHILD_SUBREAPER: control orphan reparenting
/// - PR_MCE_KILL: set the machine check exception policy
/// - PR_SET_MM options: set various memory management options (start/end code/data/brk/stack)
pub fn sys_prctl(
    option: u32,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) -> AxResult<isize> {
    use linux_raw_sys::prctl::*;

    debug!("sys_prctl <= option: {option}, args: {arg2}, {arg3}, {arg4}, {arg5}");

    match option {
        PR_SET_NAME => {
            let s = vm_load_string(arg2 as *const c_char)?;
            current().set_name(&s);
        }
        PR_GET_NAME => {
            let name = current().name();
            let len = name.len().min(15);
            let mut buf = [0; 16];
            buf[..len].copy_from_slice(&name.as_bytes()[..len]);
            vm_write_slice(arg2 as _, &buf)?;
        }
        PR_SET_PDEATHSIG => {
            let sig = arg2 as u32;
            if sig > 64 {
                return Err(AxError::InvalidInput);
            }
            current().as_thread().set_pdeathsig(sig);
        }
        PR_GET_PDEATHSIG => {
            let sig = current().as_thread().pdeathsig() as i32;
            (arg2 as *mut i32).vm_write(sig)?;
        }
        PR_SET_CHILD_SUBREAPER => {
            current()
                .as_thread()
                .proc_data
                .proc
                .set_child_subreaper(arg2 != 0);
        }
        PR_GET_CHILD_SUBREAPER => {
            let enabled = if current().as_thread().proc_data.proc.is_child_subreaper() {
                1
            } else {
                0
            };
            (arg2 as *mut i32).vm_write(enabled)?;
        }
        PR_GET_KEEPCAPS => {
            if arg2 != 0 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(AxError::InvalidInput);
            }
            return Ok(current().as_thread().cred().keep_caps as isize);
        }
        PR_SET_KEEPCAPS => {
            if arg2 > 1 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(AxError::InvalidInput);
            }
            let thread_ref = current();
            let thread = thread_ref.as_thread();
            let mut new = (*thread.cred()).clone();
            new.keep_caps = arg2 != 0;
            thread.set_cred(new);
        }
        PR_CAPBSET_READ => {
            // Query whether a capability is still present in the bounding set.
            if arg2 > CAP_LAST_CAP as usize {
                return Err(AxError::InvalidInput);
            }
            let bit = cap_bit(arg2 as u32)?;
            let cred = current().as_thread().cred();
            return Ok(((cred.cap_bounding & bit) != 0) as isize);
        }
        PR_CAPBSET_DROP => {
            // Permanently drop a capability from this thread's bounding set.
            // Linux requires CAP_SETPCAP for this operation.
            if arg2 > CAP_LAST_CAP as usize || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(AxError::InvalidInput);
            }
            let thread_ref = current();
            let thread = thread_ref.as_thread();
            let old = thread.cred();
            if !old.has_cap_setpcap() {
                return Err(AxError::OperationNotPermitted);
            }
            let bit = cap_bit(arg2 as u32)?;
            let mut new = (*old).clone();
            new.cap_bounding &= !bit;
            new.cap_ambient &= !bit;
            new.sanitize_capabilities();
            thread.set_cred(new);
        }
        PR_CAP_AMBIENT => {
            // Manage the ambient capability set.  Ambient capabilities are
            // constrained to permitted & inheritable by `sanitize_capabilities`.
            let thread_ref = current();
            let thread = thread_ref.as_thread();
            let old = thread.cred();
            match arg2 as u32 {
                PR_CAP_AMBIENT_IS_SET => {
                    if arg3 > CAP_LAST_CAP as usize || arg4 != 0 || arg5 != 0 {
                        return Err(AxError::InvalidInput);
                    }
                    let bit = cap_bit(arg3 as u32)?;
                    return Ok(((old.cap_ambient & bit) != 0) as isize);
                }
                PR_CAP_AMBIENT_RAISE => {
                    if arg3 > CAP_LAST_CAP as usize || arg4 != 0 || arg5 != 0 {
                        return Err(AxError::InvalidInput);
                    }
                    let bit = cap_bit(arg3 as u32)?;
                    if old.cap_permitted & bit == 0 || old.cap_inheritable & bit == 0 {
                        return Err(AxError::OperationNotPermitted);
                    }
                    let mut new = (*old).clone();
                    new.cap_ambient |= bit;
                    new.sanitize_capabilities();
                    thread.set_cred(new);
                }
                PR_CAP_AMBIENT_LOWER => {
                    if arg3 > CAP_LAST_CAP as usize || arg4 != 0 || arg5 != 0 {
                        return Err(AxError::InvalidInput);
                    }
                    let bit = cap_bit(arg3 as u32)?;
                    let mut new = (*old).clone();
                    new.cap_ambient &= !bit;
                    thread.set_cred(new);
                }
                PR_CAP_AMBIENT_CLEAR_ALL => {
                    if arg3 != 0 || arg4 != 0 || arg5 != 0 {
                        return Err(AxError::InvalidInput);
                    }
                    let mut new = (*old).clone();
                    new.cap_ambient = 0;
                    thread.set_cred(new);
                }
                _ => return Err(AxError::InvalidInput),
            }
        }
        PR_GET_DUMPABLE => {
            // man 2 prctl PR_GET_DUMPABLE: returns current dumpable value
            // (0=SUID_DUMP_DISABLE, 1=SUID_DUMP_USER, 2=SUID_DUMP_ROOT).
            return Ok(current().as_thread().proc_data.dumpable() as isize);
        }
        PR_SET_DUMPABLE => {
            // man 2 prctl PR_SET_DUMPABLE: arg2 must be SUID_DUMP_DISABLE (0)
            // or SUID_DUMP_USER (1); attempt to set SUID_DUMP_ROOT (2) returns
            // EINVAL (only kernel internally sets 2 on suid/sgid binary exec).
            //
            // Validate on the raw `usize` to reject high-bit-set values like
            // `0x1_0000_0001UL` that would otherwise truncate to 1 and falsely
            // succeed. Linux rejects such inputs with EINVAL.
            if arg2 != 0 && arg2 != 1 {
                return Err(AxError::InvalidInput);
            }
            current().as_thread().proc_data.set_dumpable(arg2 as i32);
        }
        PR_SET_SECCOMP => {
            if arg4 != 0 || arg5 != 0 {
                return Err(AxError::InvalidInput);
            }
            crate::syscall::sys_seccomp(arg2 as u32, 0, arg3 as *const ())?;
        }
        PR_MCE_KILL => {}
        PR_SET_NO_NEW_PRIVS => {
            if arg2 != 1 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(AxError::InvalidInput);
            }
            current().as_thread().set_no_new_privs();
        }
        PR_GET_NO_NEW_PRIVS => {
            return Ok(current().as_thread().no_new_privs() as isize);
        }
        PR_SET_THP_DISABLE => {
            // Linux reserves arg4/arg5 for this option; non-zero values are invalid.
            if arg4 != 0 || arg5 != 0 {
                return Err(AxError::InvalidInput);
            }
            // StarryOS does not implement transparent huge pages, but userspace
            // may use this prctl as a compatibility hint and query it later.
            // Linux returns 0, 1, or 3 from PR_GET_THP_DISABLE:
            //   0: enabled, 1: disabled, 3: disabled except advised mappings.
            let thp_disable = match (arg2, arg3) {
                (0, 0) => 0,
                (0, _) => return Err(AxError::InvalidInput),
                (_, 0) => 1,
                (_, PR_THP_DISABLE_EXCEPT_ADVISED) => 1 | PR_THP_DISABLE_EXCEPT_ADVISED,
                _ => return Err(AxError::InvalidInput),
            };
            current()
                .as_thread()
                .proc_data
                .set_thp_disable(thp_disable as u32);
        }
        PR_GET_THP_DISABLE => {
            // PR_GET_THP_DISABLE takes no additional arguments and returns the
            // process-local state recorded by PR_SET_THP_DISABLE.
            if arg2 != 0 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(AxError::InvalidInput);
            }
            return Ok(current().as_thread().proc_data.thp_disable() as isize);
        }
        PR_SET_MM => {
            // not implemented; but avoid annoying warnings
            return Err(AxError::InvalidInput);
        }
        PR_SET_VMA => {
            if arg2 == PR_SET_VMA_ANON_NAME as usize {
                return Ok(0);
            }
            return Err(AxError::InvalidInput);
        }
        _ => {
            warn!("sys_prctl: unsupported option {option}");
            return Err(AxError::InvalidInput);
        }
    }

    Ok(0)
}
