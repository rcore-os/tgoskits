use alloc::{string::String, vec::Vec};

use kbpf_basic::{BpfResult, KernelAuxiliaryOps, map::UnifiedMap, preprocessor::EbpfInst};

#[derive(Clone)]
pub struct StarryEbpfInst;

impl EbpfInst for StarryEbpfInst {
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

pub struct StarryAuxImpl;

impl KernelAuxiliaryOps for StarryAuxImpl {
    fn get_unified_map_from_ptr<F, R>(_ptr: *const u8, _func: F) -> BpfResult<R>
    where
        F: FnOnce(&mut UnifiedMap) -> BpfResult<R>,
    {
        let map = unsafe { &mut *(_ptr as *mut UnifiedMap) };
        _func(map)
    }

    fn get_unified_map_from_fd<F, R>(_map_fd: u32, _func: F) -> BpfResult<R>
    where
        F: FnOnce(&mut UnifiedMap) -> BpfResult<R>,
    {
        let mut guard = crate::ebpf::BPF_GLOBAL.lock();
        if let Some(map) = guard.maps.get_mut(&_map_fd) {
            _func(map)
        } else {
            Err(ax_errno::LinuxError::EBADF)
        }
    }

    fn get_unified_map_ptr_from_fd(_map_fd: u32) -> BpfResult<*const u8> {
        let mut guard = crate::ebpf::BPF_GLOBAL.lock();
        if let Some(map) = guard.maps.get_mut(&_map_fd) {
            Ok(map as *mut _ as *const u8)
        } else {
            Err(ax_errno::LinuxError::EBADF)
        }
    }

    #[allow(refining_impl_trait)]
    fn translate_instruction(_instruction: Vec<u8>) -> BpfResult<Vec<StarryEbpfInst>> {
        // TODO: Implement eBPF instruction translation if custom representation is needed.
        Err(ax_errno::LinuxError::EPERM)
    }

    fn copy_from_user(_src: *const u8, _size: usize, _dst: &mut [u8]) -> BpfResult<()> {
        let ret = unsafe { ax_runtime::hal::cpu::asm::user_copy(_dst.as_mut_ptr(), _src, _size) };
        if ret == 0 {
            Ok(())
        } else {
            Err(ax_errno::LinuxError::EFAULT)
        }
    }

    fn copy_to_user(_dest: *mut u8, _size: usize, _src: &[u8]) -> BpfResult<()> {
        let ret = unsafe { ax_runtime::hal::cpu::asm::user_copy(_dest, _src.as_ptr(), _size) };
        if ret == 0 {
            Ok(())
        } else {
            Err(ax_errno::LinuxError::EFAULT)
        }
    }

    fn current_cpu_id() -> u32 {
        ax_runtime::hal::percpu::this_cpu_id() as u32
    }

    fn perf_event_output(
        _ctx: *mut core::ffi::c_void,
        _fd: u32,
        _flags: u32,
        _data: &[u8],
    ) -> BpfResult<()> {
        // TODO: Implement perf_event_output for eBPF
        // This is typically used by `bpf_perf_event_output` helper to push data to user space.
        Err(ax_errno::LinuxError::EPERM)
    }

    fn string_from_user_cstr(_ptr: *const u8) -> BpfResult<String> {
        // TODO: Read a null-terminated string from user space safely.
        Err(ax_errno::LinuxError::EPERM)
    }

    fn ebpf_write_str(_str: &str) -> BpfResult<()> {
        // TODO: Helper for eBPF to print a string to kernel log.
        Err(ax_errno::LinuxError::EPERM)
    }

    fn ebpf_time_ns() -> BpfResult<u64> {
        Ok(ax_runtime::hal::time::monotonic_time_nanos())
    }

    fn alloc_page() -> BpfResult<usize> {
        // TODO: Allocate physical pages for eBPF maps/programs.
        Err(ax_errno::LinuxError::EPERM)
    }

    fn free_page(_phys_addr: usize) {
        // TODO: Free physical pages.
    }

    fn vmap(_phys_addrs: &[usize]) -> BpfResult<usize> {
        // TODO: Map physical pages into contiguous kernel virtual memory.
        Err(ax_errno::LinuxError::EPERM)
    }

    fn unmap(_vaddr: usize) {
        // TODO: Unmap kernel virtual memory.
    }
}
