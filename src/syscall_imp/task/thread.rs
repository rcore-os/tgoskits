use core::{
    ffi::{c_char, c_int},
    ptr,
};

use axerrno::LinuxError;
use axtask::{TaskExtRef, current, yield_now};
use num_enum::TryFromPrimitive;

use crate::{
    ctypes::{WaitFlags, WaitStatus},
    ptr::{PtrWrapper, UserConstPtr, UserPtr},
    syscall_body,
    syscall_imp::read_path_str,
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

pub(crate) fn sys_getpid() -> i32 {
    syscall_body!(sys_getpid, {
        Ok(axtask::current().task_ext().proc_id as c_int)
    })
}

pub(crate) fn sys_getppid() -> i32 {
    syscall_body!(sys_getppid, {
        Ok(axtask::current().task_ext().get_parent() as c_int)
    })
}

pub(crate) fn sys_exit(status: i32) -> ! {
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

pub(crate) fn sys_exit_group(status: i32) -> ! {
    warn!("Temporarily replace sys_exit_group with sys_exit");
    axtask::exit(status);
}

/// To set the clear_child_tid field in the task extended data.
///
/// The set_tid_address() always succeeds
pub(crate) fn sys_set_tid_address(tid_ptd: UserConstPtr<i32>) -> isize {
    syscall_body!(sys_set_tid_address, {
        let curr = current();
        curr.task_ext()
            .set_clear_child_tid(tid_ptd.address().as_ptr() as _);
        Ok(curr.id().as_u64() as isize)
    })
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn sys_arch_prctl(code: i32, addr: u64) -> isize {
    use axerrno::LinuxError;
    syscall_body!(sys_arch_prctl, {
        match ArchPrctlCode::try_from(code) {
            // TODO: check the legality of the address
            Ok(ArchPrctlCode::SetFs) => {
                unsafe {
                    axhal::arch::write_thread_pointer(addr as usize);
                }
                Ok(0)
            }
            Ok(ArchPrctlCode::GetFs) => {
                unsafe {
                    *(addr as *mut u64) = axhal::arch::read_thread_pointer() as u64;
                }
                Ok(0)
            }
            Ok(ArchPrctlCode::SetGs) => {
                unsafe {
                    x86::msr::wrmsr(x86::msr::IA32_KERNEL_GSBASE, addr);
                }
                Ok(0)
            }
            Ok(ArchPrctlCode::GetGs) => {
                unsafe {
                    *(addr as *mut u64) = x86::msr::rdmsr(x86::msr::IA32_KERNEL_GSBASE);
                }
                Ok(0)
            }
            _ => Err(LinuxError::ENOSYS),
        }
    })
}

pub(crate) fn sys_clone(
    flags: usize,
    user_stack: usize,
    ptid: usize,
    arg3: usize,
    arg4: usize,
) -> isize {
    syscall_body!(sys_clone, {
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
    })
}

pub(crate) fn sys_wait4(pid: i32, exit_code_ptr: UserPtr<i32>, option: u32) -> isize {
    let option_flag = WaitFlags::from_bits(option).unwrap();
    syscall_body!(sys_wait4, {
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
    })
}

pub fn sys_execve(
    path: UserConstPtr<c_char>,
    argv: UserConstPtr<usize>,
    envp: UserConstPtr<usize>,
) -> isize {
    syscall_body!(sys_execve, {
        let path_str = read_path_str(path)?;

        info!("execve: {:?}", path_str);
        if path_str.split('/').filter(|s| !s.is_empty()).count() > 1 {
            info!("Multi-level directories are not supported");
            return Err::<isize, _>(LinuxError::EINVAL);
        }

        let argv = argv.get()?;
        let envp = envp.get()?;
        let argv_valid = unsafe { argv.is_null() || *argv == 0 };
        let envp_valid = unsafe { envp.is_null() || *envp == 0 };

        if !argv_valid {
            info!("argv is not supported");
        }

        if !envp_valid {
            info!("envp is not supported");
        }

        if let Err(e) = crate::task::exec(path_str) {
            error!("Failed to exec: {:?}", e);
            return Err(LinuxError::ENOSYS);
        }

        unreachable!("execve should never return");
    })
}
