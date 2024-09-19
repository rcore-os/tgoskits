#![allow(dead_code)]

use arceos_posix_api as api;
use axerrno::{AxError, LinuxError};
use axhal::arch::{TrapFrame, UspaceContext};
use axhal::paging::MappingFlags;
use axhal::trap::{register_trap_handler, SYSCALL};
use axtask::{current, TaskExtRef};
use memory_addr::{VirtAddr, VirtAddrRange};
use num_enum::TryFromPrimitive;

const SYS_READ: usize = 0;
const SYS_WRITE: usize = 1;
const SYS_MMAP: usize = 9;
const SYS_IOCTL: usize = 16;
const SYS_WRITEV: usize = 20;
const SYS_SCHED_YIELD: usize = 24;
const SYS_NANOSLEEP: usize = 35;
const SYS_GETPID: usize = 39;
const SYS_CLONE: usize = 56;
const SYS_FORK: usize = 57;
const SYS_EXECVE: usize = 59;
const SYS_EXIT: usize = 60;
const SYS_ARCH_PRCTL: usize = 158;
const SYS_SET_TID_ADDRESS: usize = 218;
const SYS_CLOCK_GETTIME: usize = 228;
const SYS_CLOCK_NANOSLEEP: usize = 230;
const SYS_EXITGROUP: usize = 231;

bitflags::bitflags! {
    #[derive(Debug)]
    /// permissions for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    pub struct MmapProt: i32 {
        /// Page can be read.
        const PROT_READ = 1 << 0;
        /// Page can be written.
        const PROT_WRITE = 1 << 1;
        /// Page can be executed.
        const PROT_EXEC = 1 << 2;
    }
}

impl From<MmapProt> for MappingFlags {
    fn from(value: MmapProt) -> Self {
        let mut flags = MappingFlags::USER;
        if value.contains(MmapProt::PROT_READ) {
            flags |= MappingFlags::READ;
        }
        if value.contains(MmapProt::PROT_WRITE) {
            flags |= MappingFlags::WRITE;
        }
        if value.contains(MmapProt::PROT_EXEC) {
            flags |= MappingFlags::EXECUTE;
        }
        flags
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    /// flags for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    pub struct MmapFlags: i32 {
        /// Share changes
        const MAP_SHARED = 1 << 0;
        /// Changes private; copy pages on write.
        const MAP_PRIVATE = 1 << 1;
        /// Map address must be exactly as requested, no matter whether it is available.
        const MAP_FIXED = 1 << 4;
        /// Don't use a file.
        const MAP_ANONYMOUS = 1 << 5;
        /// Don't check for reservations.
        const MAP_NORESERVE = 1 << 14;
        /// Allocation is for a stack.
        const MAP_STACK = 0x20000;
    }
}

#[derive(Debug, Eq, PartialEq, TryFromPrimitive)]
#[repr(i32)]
/// ARCH_PRCTL codes
pub enum ArchPrctlCode {
    /// Set the FS segment base
    ArchSetFs = 0x1002,
    /// Get the FS segment base
    ArchGetFs = 0x1003,
    /// Set the GS segment base
    ArchSetGs = 0x1001,
    /// Get the GS segment base
    ArchGetGs = 0x1004,
}

#[cfg(target_arch = "x86_64")]
fn sys_arch_prctl(code: i32, addr: *mut usize) -> isize {
    match ArchPrctlCode::try_from(code) {
        // TODO: check the legality of the address
        Ok(ArchPrctlCode::ArchSetFs) => {
            unsafe {
                axhal::arch::write_thread_pointer(*addr);
            }
            0
        }
        Ok(ArchPrctlCode::ArchGetFs) => {
            unsafe {
                *addr = axhal::arch::read_thread_pointer();
            }
            0
        }
        Ok(ArchPrctlCode::ArchSetGs) | Ok(ArchPrctlCode::ArchGetGs) => todo!(),
        _ => -LinuxError::EINVAL.code() as isize,
    }
}

/// To set the clear_child_tid field in the task extended data.
///
/// The set_tid_address() always succeeds
fn sys_set_tid_address(tid_ptd: *const i32) -> isize {
    let curr = current();
    curr.task_ext().set_clear_child_tid(tid_ptd as _);
    curr.id().as_u64() as isize
}

fn sys_exit(status: i32) -> ! {
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

fn mmap(addr: *mut usize, length: usize, prot: i32, flags: i32) -> Result<VirtAddr, LinuxError> {
    let curr = current();
    let curr_ext = curr.task_ext();
    let mut aspace = curr_ext.aspace.lock();
    let permiss_flags = MmapProt::from_bits_truncate(prot);
    // TODO: check illegal flags for mmap
    // An example is the flags contained none of MAP_PRIVATE, MAP_SHARED, or MAP_SHARED_VALIDATE.
    let map_flags = MmapFlags::from_bits_truncate(flags);

    let start_addr = if map_flags.contains(MmapFlags::MAP_FIXED) {
        VirtAddr::from(addr as usize)
    } else {
        aspace
            .find_free_area(
                VirtAddr::from(addr as usize),
                length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
            )
            .or(aspace.find_free_area(
                aspace.base(),
                length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
            ))
            .ok_or(LinuxError::ENOMEM)?
    };

    aspace
        .map_alloc(start_addr, length, permiss_flags.into(), false)
        .map_err(<AxError as From<_>>::from)?;

    Ok(start_addr)
}

fn sys_mmap(addr: *mut usize, length: usize, prot: i32, flags: i32) -> isize {
    match mmap(addr, length, prot, flags) {
        Ok(addr) => addr.as_usize() as isize,
        Err(err) => -(err.code() as isize),
    }
}

fn sys_clone(tf: &TrapFrame, newsp: usize) -> usize {
    let aspace = axtask::current().task_ext().aspace.clone();
    let mut uctx = UspaceContext::from(tf);
    uctx.set_sp(newsp);
    uctx.set_retval(0);
    let new_task = crate::task::spawn_user_task(aspace, uctx);
    new_task.id().as_u64() as usize
}

fn sys_ioctl() -> usize {
    warn!("Unimplemented syscall: SYS_IOCTL");
    0
}

#[register_trap_handler(SYSCALL)]
fn handle_syscall(tf: &TrapFrame, syscall_num: usize) -> isize {
    let ret = match syscall_num {
        SYS_READ => api::sys_read(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_WRITE => api::sys_write(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_MMAP => sys_mmap(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        SYS_IOCTL => sys_ioctl() as _,
        SYS_WRITEV => unsafe { api::sys_writev(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _) },
        SYS_SCHED_YIELD => api::sys_sched_yield() as isize,
        SYS_NANOSLEEP => unsafe { api::sys_nanosleep(tf.arg0() as _, tf.arg1() as _) as _ },
        SYS_GETPID => api::sys_getpid() as isize,
        SYS_EXIT => axtask::exit(tf.arg0() as _),
        SYS_ARCH_PRCTL => sys_arch_prctl(tf.arg0() as _, tf.arg1() as _),
        SYS_SET_TID_ADDRESS => sys_set_tid_address(tf.arg0() as _),
        SYS_CLOCK_GETTIME => unsafe { api::sys_clock_gettime(tf.arg0() as _, tf.arg1() as _) as _ },
        SYS_CLOCK_NANOSLEEP => unsafe {
            // TODO: port to the posix api
            api::sys_nanosleep(tf.arg2() as _, tf.arg3() as _) as _
        },
        SYS_EXITGROUP => sys_exit(tf.arg0() as _),
        _ => {
            warn!("Unimplemented syscall: {}", syscall_num);
            axtask::exit(LinuxError::ENOSYS as _)
        }
    };
    ret
}
