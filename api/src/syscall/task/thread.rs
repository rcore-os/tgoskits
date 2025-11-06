use axerrno::{AxError, AxResult};
use axtask::current;
use num_enum::TryFromPrimitive;
use starry_core::task::AsThread;

pub fn sys_getpid() -> AxResult<isize> {
    Ok(current().as_thread().proc_data.proc.pid() as _)
}

pub fn sys_getppid() -> AxResult<isize> {
    current()
        .as_thread()
        .proc_data
        .proc
        .parent()
        .ok_or(AxError::NoSuchProcess)
        .map(|p| p.pid() as _)
}

pub fn sys_gettid() -> AxResult<isize> {
    Ok(current().id().as_u64() as _)
}

/// ARCH_PRCTL codes
///
/// It is only avaliable on x86_64, and is not convenient
/// to generate automatically via c_to_rust binding.
#[derive(Debug, Eq, PartialEq, TryFromPrimitive)]
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
    curr.as_thread().set_clear_child_tid(clear_child_tid);
    Ok(curr.id().as_u64() as isize)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_arch_prctl(
    uctx: &mut axhal::uspace::UserContext,
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
        ArchPrctlCode::SetCpuid => Err(axerrno::AxError::NoSuchDevice),
    }
}
