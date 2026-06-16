use ax_errno::{AxError, AxResult};
use ax_task::current;

use crate::task::AsThread;

#[inline(never)]
pub fn sys_getpid() -> AxResult<isize> {
    let curr = current();
    let thr = curr.as_thread();
    let global_pid = thr.proc_data.proc.pid() as u64;
    let nsproxy = thr.proc_data.nsproxy.lock();
    let local = nsproxy.pid_ns.lock().local_pid(global_pid);
    drop(nsproxy);
    if let Some(local) = local {
        Ok(local as isize)
    } else {
        Ok(global_pid as isize)
    }
}

pub fn sys_getppid() -> AxResult<isize> {
    let curr = current();
    let thr = curr.as_thread();
    let parent = thr.proc_data.proc.parent().ok_or(AxError::NoSuchProcess)?;
    let parent_global_pid = parent.pid() as u64;
    let nsproxy = thr.proc_data.nsproxy.lock();
    match nsproxy.pid_ns.lock().local_pid(parent_global_pid) {
        Some(local) => Ok(local as isize),
        None => Ok(0),
    }
}

pub fn sys_gettid() -> AxResult<isize> {
    // `Thread::tid` rather than the scheduler ID: after a non-leader
    // `execve` they differ (the calling thread inherits the leader's TID
    // so that `gettid() == getpid()` holds in the new image).
    Ok(current().as_thread().tid() as _)
}

/// `getcpu(2)`: report the CPU and NUMA node the caller is running on.
///
/// glibc's `sched_getcpu` and NUMA-aware allocators query this. We report the
/// current CPU id and node 0 (single NUMA node); the obsolete `tcache` arg is
/// ignored. Either pointer may be NULL.
pub fn sys_getcpu(cpu: *mut u32, node: *mut u32, _tcache: usize) -> AxResult<isize> {
    use ax_runtime::hal::percpu::this_cpu_id;
    use starry_vm::VmMutPtr;

    if !cpu.is_null() {
        cpu.vm_write(this_cpu_id() as u32)?;
    }
    if !node.is_null() {
        node.vm_write(0)?;
    }
    Ok(0)
}

/// ARCH_PRCTL codes
///
/// It is only available on x86_64, and is not convenient
/// to generate automatically via c_to_rust binding.
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Eq, PartialEq, num_enum::TryFromPrimitive)]
#[repr(i32)]
enum ArchPrctlCode {
    /// Set the GS segment base
    SetGs    = 0x1001,
    /// Set the FS segment base
    SetFs    = 0x1002,
    /// Get the FS segment base
    GetFs    = 0x1003,
    /// Get the GS segment base
    GetGs    = 0x1004,
    /// The setting of the flag manipulated by ARCH_SET_CPUID
    GetCpuid = 0x1011,
    /// Enable (addr != 0) or disable (addr == 0) the cpuid instruction for the
    /// calling thread.
    SetCpuid = 0x1012,
}

/// To set the clear_child_tid field in the task extended data.
///
/// The set_tid_address() always succeeds
pub fn sys_set_tid_address(clear_child_tid: usize) -> AxResult<isize> {
    let curr = current();
    let thr = curr.as_thread();
    thr.set_clear_child_tid(clear_child_tid);
    Ok(thr.tid() as isize)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_arch_prctl(
    uctx: &mut ax_runtime::hal::cpu::uspace::UserContext,
    code: i32,
    addr: usize,
) -> AxResult<isize> {
    use starry_vm::VmMutPtr;

    let code = ArchPrctlCode::try_from(code).map_err(|_| AxError::InvalidInput)?;
    debug!("sys_arch_prctl: code = {code:?}, addr = {addr:#x}");

    match code {
        // According to Linux implementation, SetFs & SetGs does not return
        // error at all
        ArchPrctlCode::GetFs => {
            (addr as *mut usize).vm_write(uctx.tls())?;
            Ok(0)
        }
        ArchPrctlCode::SetFs => {
            uctx.set_tls(addr);
            Ok(0)
        }
        ArchPrctlCode::GetGs => {
            (addr as *mut usize).vm_write(uctx.gs_base as _)?;
            Ok(0)
        }
        ArchPrctlCode::SetGs => {
            uctx.gs_base = addr as _;
            Ok(0)
        }
        ArchPrctlCode::GetCpuid => Ok(0),
        ArchPrctlCode::SetCpuid => Err(ax_errno::AxError::NoSuchDevice),
    }
}
