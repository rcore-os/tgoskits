use ax_task::current;

use crate::{
    StarryError, StarryResult,
    task::{AsThread, current_pid_view},
};

#[inline(never)]
pub fn sys_getpid() -> StarryResult<isize> {
    let curr = current();
    let thr = curr.as_thread();
    current_pid_view()
        .visible_process_number(&thr.proc_data.identity())
        .map(|pid| pid.get() as isize)
        .ok_or(StarryError::NoSuchProcess)
}

pub fn sys_getppid() -> StarryResult<isize> {
    let curr = current();
    let thr = curr.as_thread();
    let parent = thr
        .proc_data
        .proc
        .parent()
        .ok_or(StarryError::NoSuchProcess)?;
    Ok(current_pid_view()
        .visible_process_number(&parent.identity())
        .map_or(0, |pid| pid.get() as isize))
}

pub fn sys_gettid() -> StarryResult<isize> {
    // `Thread::tid` rather than the scheduler ID: after a non-leader
    // `execve` they differ (the calling thread inherits the leader's TID
    // so that `gettid() == getpid()` holds in the new image).
    Ok(current().as_thread().user_tid().get() as _)
}

/// `getcpu(2)`: report the CPU and NUMA node the caller is running on.
///
/// glibc's `sched_getcpu` and NUMA-aware allocators query this. We report the
/// current CPU id and node 0 (single NUMA node); the obsolete `tcache` arg is
/// ignored. Either pointer may be NULL.
pub fn sys_getcpu(cpu: *mut u32, node: *mut u32, _tcache: usize) -> StarryResult<isize> {
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
pub fn sys_set_tid_address(clear_child_tid: usize) -> StarryResult<isize> {
    let curr = current();
    let thr = curr.as_thread();
    thr.set_clear_child_tid(clear_child_tid);
    Ok(thr.user_tid().get() as isize)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_arch_prctl(
    uctx: &mut ax_runtime::hal::cpu::uspace::UserContext,
    code: i32,
    addr: usize,
) -> StarryResult<isize> {
    use starry_vm::VmMutPtr;

    let code = ArchPrctlCode::try_from(code).map_err(|_| StarryError::InvalidInput)?;
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
        // Linux get_cpuid_mode() returns 1 (ARCH_CPUID_ENABLE) when the CPUID
        // instruction is enabled for the thread and 0 when it faults. StarryOS
        // never installs CPUID faulting, so CPUID is always enabled and GET must
        // report 1 rather than a hardcoded 0. SET stays ENODEV: without faulting
        // support Linux rejects every requested mode.
        ArchPrctlCode::GetCpuid => Ok(1),
        ArchPrctlCode::SetCpuid => Err(crate::StarryError::NoSuchDevice),
    }
}

#[cfg(all(test, not(axtest)))]
fn thread_arch_prctl_code_rules_hold_for_test() -> bool {
    // Test ArchPrctlCode enum values
    #[cfg(target_arch = "x86_64")]
    {
        assert!(ArchPrctlCode::SetGs as i32 == 0x1001);
        assert!(ArchPrctlCode::SetFs as i32 == 0x1002);
        assert!(ArchPrctlCode::GetFs as i32 == 0x1003);
        assert!(ArchPrctlCode::GetGs as i32 == 0x1004);
        assert!(ArchPrctlCode::GetCpuid as i32 == 0x1011);
        assert!(ArchPrctlCode::SetCpuid as i32 == 0x1012);

        // Test TryFromPrimitive
        use num_enum::TryFromPrimitive;
        assert!(ArchPrctlCode::try_from_primitive(0x1001).is_ok());
        assert!(ArchPrctlCode::try_from_primitive(0x1002).is_ok());
        assert!(ArchPrctlCode::try_from_primitive(0xFFFF).is_err());
    }

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn thread_arch_prctl_code_rules_hold() {
        assert!(super::thread_arch_prctl_code_rules_hold_for_test());
    }
}
