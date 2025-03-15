use core::{ffi::c_char, ptr};

use alloc::vec::Vec;
use axerrno::{LinuxError, LinuxResult};
use axtask::{TaskExtRef, current, yield_now};
use macro_rules_attribute::apply;
use num_enum::TryFromPrimitive;

use crate::{
    ctypes::{WaitFlags, WaitStatus},
    ptr::{PtrWrapper, UserConstPtr, UserPtr},
    syscall_imp::syscall_instrument,
    task::wait_pid,
};

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

#[apply(syscall_instrument)]
pub fn sys_getpid() -> LinuxResult<isize> {
    Ok(axtask::current().task_ext().proc_id as _)
}

#[apply(syscall_instrument)]
pub fn sys_getppid() -> LinuxResult<isize> {
    Ok(axtask::current().task_ext().get_parent() as _)
}

pub fn sys_exit(status: i32) -> ! {
    let curr = current();
    let clear_child_tid = curr.task_ext().clear_child_tid() as *mut i32;
    if !clear_child_tid.is_null() {
        // TODO: check whether the address is valid
        unsafe {
            // TODO: Encapsulate all operations that access user-mode memory into a unified function
            *(clear_child_tid) = 0;
        }
        // TODO: wake up threads, which are blocked by futex, and waiting for the address pointed by clear_child_tid
    }
    axtask::exit(status);
}

pub fn sys_exit_group(status: i32) -> ! {
    warn!("Temporarily replace sys_exit_group with sys_exit");
    axtask::exit(status);
}

/// To set the clear_child_tid field in the task extended data.
///
/// The set_tid_address() always succeeds
#[apply(syscall_instrument)]
pub fn sys_set_tid_address(tid_ptd: UserConstPtr<i32>) -> LinuxResult<isize> {
    let curr = current();
    curr.task_ext()
        .set_clear_child_tid(tid_ptd.address().as_ptr() as _);
    Ok(curr.id().as_u64() as isize)
}

#[cfg(target_arch = "x86_64")]
#[apply(syscall_instrument)]
pub fn sys_arch_prctl(code: i32, addr: UserPtr<u64>) -> LinuxResult<isize> {
    use axerrno::LinuxError;
    match ArchPrctlCode::try_from(code).map_err(|_| LinuxError::EINVAL)? {
        // According to Linux implementation, SetFs & SetGs does not return
        // error at all
        ArchPrctlCode::SetFs => {
            unsafe {
                axhal::arch::write_thread_pointer(addr.address().as_usize());
            }
            Ok(0)
        }
        ArchPrctlCode::SetGs => {
            unsafe {
                x86::msr::wrmsr(x86::msr::IA32_KERNEL_GSBASE, addr.address().as_usize() as _);
            }
            Ok(0)
        }
        ArchPrctlCode::GetFs => {
            unsafe {
                *addr.get()? = axhal::arch::read_thread_pointer() as u64;
            }
            Ok(0)
        }

        ArchPrctlCode::GetGs => {
            unsafe {
                *addr.get()? = x86::msr::rdmsr(x86::msr::IA32_KERNEL_GSBASE);
            }
            Ok(0)
        }
        ArchPrctlCode::GetCpuid => Ok(0),
        ArchPrctlCode::SetCpuid => Err(LinuxError::ENODEV),
    }
}

#[apply(syscall_instrument)]
pub fn sys_clone(
    flags: usize,
    user_stack: usize,
    ptid: usize,
    arg3: usize,
    arg4: usize,
) -> LinuxResult<isize> {
    let tls = arg3;
    let ctid = arg4;

    let stack = if user_stack == 0 {
        None
    } else {
        Some(user_stack)
    };

    let curr_task = current();

    if let Ok(new_task_id) = curr_task
        .task_ext()
        .clone_task(flags, stack, ptid, tls, ctid)
    {
        Ok(new_task_id as isize)
    } else {
        Err(LinuxError::ENOMEM)
    }
}

#[apply(syscall_instrument)]
pub fn sys_wait4(pid: i32, exit_code_ptr: UserPtr<i32>, option: u32) -> LinuxResult<isize> {
    let option_flag = WaitFlags::from_bits(option).unwrap();
    let exit_code_ptr = exit_code_ptr.nullable(UserPtr::get)?;
    loop {
        let answer = wait_pid(pid, exit_code_ptr.unwrap_or_else(ptr::null_mut));
        match answer {
            Ok(pid) => {
                return Ok(pid as isize);
            }
            Err(status) => match status {
                WaitStatus::NotExist => {
                    return Err(LinuxError::ECHILD);
                }
                WaitStatus::Running => {
                    if option_flag.contains(WaitFlags::WNOHANG) {
                        return Ok(0);
                    } else {
                        yield_now();
                    }
                }
                _ => {
                    panic!("Shouldn't reach here!");
                }
            },
        }
    }
}

#[apply(syscall_instrument)]
pub fn sys_execve(
    path: UserConstPtr<c_char>,
    argv: UserConstPtr<usize>,
    envp: UserConstPtr<usize>,
) -> LinuxResult<isize> {
    let path_str = path.get_as_str()?;

    let args = argv
        .get_as_null_terminated()?
        .iter()
        .map(|arg| {
            UserConstPtr::<c_char>::from(*arg)
                .get_as_str()
                .map(Into::into)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let envs = envp
        .get_as_null_terminated()?
        .iter()
        .map(|env| {
            UserConstPtr::<c_char>::from(*env)
                .get_as_str()
                .map(Into::into)
        })
        .collect::<Result<Vec<_>, _>>()?;

    info!(
        "execve: path: {:?}, args: {:?}, envs: {:?}",
        path_str, args, envs
    );

    if let Err(e) = crate::task::exec(path_str, &args, &envs) {
        error!("Failed to exec: {:?}", e);
        return Err::<isize, _>(LinuxError::ENOSYS);
    }

    unreachable!("execve should never return");
}
