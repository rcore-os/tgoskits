//! Basic eBPF library providing essential functionalities for eBPF programs.
//! ! This library includes support for BPF maps, helper functions, and program
//! loading mechanisms, making it easier to develop and run eBPF programs in a
//! kernel-like environment.

#![deny(missing_docs)]
#![no_std]
#![feature(c_variadic)]
#![allow(unused)]
extern crate alloc;
use alloc::{string::String, vec::Vec};

use map::UnifiedMap;

use crate::preprocessor::EbpfInst;
pub mod helper;
pub mod linux_bpf;
pub mod map;
pub mod perf;
pub mod preprocessor;
pub mod prog;
pub mod raw_tracepoint;

/// Type alias for BPF results and errors.
pub type BpfResult<T> = axerrno::LinuxResult<T>;
/// Type alias for BPF errors.
pub type BpfError = axerrno::LinuxError;

/// PollWaiter trait for maps that support polling.
pub trait PollWaker: Send + Sync {
    /// Wake up any waiters on the map.
    fn wake_up(&self);
}

/// The KernelAuxiliaryOps trait provides auxiliary operations which should
/// be implemented by the kernel or a kernel-like environment.
pub trait KernelAuxiliaryOps: Send + Sync + 'static {
    /// Get a unified map from a pointer.
    fn get_unified_map_from_ptr<F, R>(ptr: *const u8, func: F) -> BpfResult<R>
    where
        F: FnOnce(&mut UnifiedMap) -> BpfResult<R>;
    /// Get a unified map from a file descriptor.
    fn get_unified_map_from_fd<F, R>(map_fd: u32, func: F) -> BpfResult<R>
    where
        F: FnOnce(&mut UnifiedMap) -> BpfResult<R>;
    /// Get a unified map pointer from a file descriptor.
    fn get_unified_map_ptr_from_fd(map_fd: u32) -> BpfResult<*const u8>;
    /// Translate eBPF instructions, which may involve relocating map file descriptors.
    fn translate_instruction(instruction: Vec<u8>) -> BpfResult<Vec<impl preprocessor::EbpfInst>>;
    /// Copy data from a user space pointer to a kernel space buffer.
    fn copy_from_user(src: *const u8, size: usize, dst: &mut [u8]) -> BpfResult<()>;
    /// Copy data from a kernel space buffer to a user space pointer.
    fn copy_to_user(dest: *mut u8, size: usize, src: &[u8]) -> BpfResult<()>;
    /// Get the current CPU ID.
    fn current_cpu_id() -> u32;
    /// Output some data to a perf buf
    fn perf_event_output(
        ctx: *mut core::ffi::c_void,
        fd: u32,
        flags: u32,
        data: &[u8],
    ) -> BpfResult<()>;
    /// Read a string from a user space pointer.
    fn string_from_user_cstr(ptr: *const u8) -> BpfResult<String>;
    /// For ebpf print helper functions
    fn ebpf_write_str(str: &str) -> BpfResult<()>;
    /// For ebpf ktime helper functions
    fn ebpf_time_ns() -> BpfResult<u64>;

    /// Allocate pages in kernel space. Return the physical address of the allocated page.
    fn alloc_page() -> BpfResult<usize>;
    /// Free the allocated page in kernel space.
    fn free_page(phys_addr: usize);
    /// Create a virtual mapping for the given physical addresses. Return the virtual address.
    fn vmap(phys_addrs: &[usize]) -> BpfResult<usize>;
    /// Unmap the given virtual address.
    fn unmap(vaddr: usize);
}

struct DummyAuxImpl;

#[derive(Clone)]
struct DummyInst;
impl EbpfInst for DummyInst {
    fn opc(&self) -> u8 {
        0
    }

    fn imm(&self) -> i32 {
        0
    }

    fn src(&self) -> u8 {
        0
    }

    fn set_imm(&mut self, _imm: i32) {}

    fn to_array(&self) -> [u8; 8] {
        [0; 8]
    }
}

impl KernelAuxiliaryOps for DummyAuxImpl {
    fn get_unified_map_from_ptr<F, R>(_ptr: *const u8, _func: F) -> BpfResult<R>
    where
        F: FnOnce(&mut UnifiedMap) -> BpfResult<R>,
    {
        Err(BpfError::EPERM)
    }

    fn get_unified_map_from_fd<F, R>(_map_fd: u32, _func: F) -> BpfResult<R>
    where
        F: FnOnce(&mut UnifiedMap) -> BpfResult<R>,
    {
        Err(BpfError::EPERM)
    }

    fn get_unified_map_ptr_from_fd(_map_fd: u32) -> BpfResult<*const u8> {
        Err(BpfError::EPERM)
    }

    fn translate_instruction(instruction: Vec<u8>) -> BpfResult<Vec<impl preprocessor::EbpfInst>> {
        Err::<Vec<DummyInst>, BpfError>(BpfError::EPERM)
    }

    fn copy_from_user(_src: *const u8, _size: usize, _dst: &mut [u8]) -> BpfResult<()> {
        Err(BpfError::EPERM)
    }

    fn copy_to_user(_dest: *mut u8, _size: usize, _src: &[u8]) -> BpfResult<()> {
        Err(BpfError::EPERM)
    }

    fn current_cpu_id() -> u32 {
        0
    }

    fn perf_event_output(
        _ctx: *mut core::ffi::c_void,
        _fd: u32,
        _flags: u32,
        _data: &[u8],
    ) -> BpfResult<()> {
        Err(BpfError::EPERM)
    }

    fn string_from_user_cstr(_ptr: *const u8) -> BpfResult<String> {
        Err(BpfError::EPERM)
    }

    fn ebpf_write_str(_str: &str) -> BpfResult<()> {
        Err(BpfError::EPERM)
    }

    fn ebpf_time_ns() -> BpfResult<u64> {
        Err(BpfError::EPERM)
    }

    fn alloc_page() -> BpfResult<usize> {
        Err(BpfError::EPERM)
    }

    fn free_page(_phys_addr: usize) {}

    fn vmap(_phys_addrs: &[usize]) -> BpfResult<usize> {
        Err(BpfError::EPERM)
    }

    fn unmap(_vaddr: usize) {}
}
