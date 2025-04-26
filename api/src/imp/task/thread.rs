use axerrno::LinuxResult;
use axtask::{TaskExtRef, current};
use macro_rules_attribute::apply;
use num_enum::TryFromPrimitive;

use crate::syscall_instrument;

#[apply(syscall_instrument)]
pub fn sys_getpid() -> LinuxResult<isize> {
    Ok(axtask::current().task_ext().thread.process().pid() as _)
}

#[apply(syscall_instrument)]
pub fn sys_getppid() -> LinuxResult<isize> {
    Ok(axtask::current()
        .task_ext()
        .thread
        .process()
        .parent()
        .unwrap()
        .pid() as _)
}

#[apply(syscall_instrument)]
pub fn sys_gettid() -> LinuxResult<isize> {
    Ok(axtask::current().id().as_u64() as _)
}

/// ARCH_PRCTL codes
///
/// It is only avaliable on x86_64, and is not convenient
/// to generate automatically via c_to_rust binding.
#[derive(Debug, Eq, PartialEq, TryFromPrimitive)]
#[repr(i32)]
enum ArchPrctlCode {
    /// Set the GS segment base
    SetGs = 0x1001,
    /// Set the FS segment base
    SetFs = 0x1002,
    /// Get the FS segment base
    GetFs = 0x1003,
    /// Get the GS segment base
    GetGs = 0x1004,
    /// The setting of the flag manipulated by ARCH_SET_CPUID
    GetCpuid = 0x1011,
    /// Enable (addr != 0) or disable (addr == 0) the cpuid instruction for the calling thread.
    SetCpuid = 0x1012,
}

/// To set the clear_child_tid field in the task extended data.
///
/// The set_tid_address() always succeeds
#[apply(syscall_instrument)]
pub fn sys_set_tid_address(clear_child_tid: usize) -> LinuxResult<isize> {
    let curr = current();
    curr.task_ext()
        .thread_data()
        .set_clear_child_tid(clear_child_tid);
    Ok(curr.id().as_u64() as isize)
}

#[cfg(target_arch = "x86_64")]
#[apply(syscall_instrument)]
pub fn sys_arch_prctl(
    tf: &mut axhal::arch::TrapFrame,
    code: i32,
    addr: usize,
) -> LinuxResult<isize> {
    use crate::ptr::{PtrWrapper, UserPtr};

    let code = ArchPrctlCode::try_from(code).map_err(|_| axerrno::LinuxError::EINVAL)?;
    debug!("sys_arch_prctl: code = {:?}, addr = {:#x}", code, addr);

    match code {
        // According to Linux implementation, SetFs & SetGs does not return
        // error at all
        ArchPrctlCode::GetFs => {
            unsafe {
                *UserPtr::from(addr).get()? = tf.tls();
            }
            Ok(0)
        }
        ArchPrctlCode::SetFs => {
            tf.set_tls(addr);
            Ok(0)
        }
        ArchPrctlCode::GetGs => {
            unsafe {
                *UserPtr::from(addr).get()? = x86::msr::rdmsr(x86::msr::IA32_KERNEL_GSBASE);
            }
            Ok(0)
        }
        ArchPrctlCode::SetGs => {
            unsafe {
                x86::msr::wrmsr(x86::msr::IA32_KERNEL_GSBASE, addr as _);
            }
            Ok(0)
        }
        ArchPrctlCode::GetCpuid => Ok(0),
        ArchPrctlCode::SetCpuid => Err(axerrno::LinuxError::ENODEV),
    }
}
