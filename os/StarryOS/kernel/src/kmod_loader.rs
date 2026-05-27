//! Loadable Kernel Module (LKM) support for StarryOS.
//!
//! Integrates the [`kmod_loader`] crate to provide `init_module` /
//! `delete_module` / `finit_module` system calls for loading .ko ELF
//! kernel modules at runtime.
//!
//! # Key Components
//!
//! - **KmodSectionMem**: Page-aligned memory with R/W/X permission control
//!   via page table manipulation. Includes null-safe Drop.
//! - **NullSectionMem**: No-op fallback when vmalloc fails.
//! - **StarryKmodHelper**: Implements `KernelModuleHelper` trait with
//!   vmalloc, resolve_symbol (stub), and cache flushing.
//!
//! # Known Limitations
//!
//! - `resolve_symbol` is a stub returning `None`; full implementation
//!   requires PR #837 (kallsyms) to be merged.
//! - `delete_module` and `finit_module` are syscall stubs.
//! - Loaded modules are kept alive via `mem::forget`; future `delete_module`
//!   will need a global registry to recover and free resources.

use alloc::{
    alloc::{Layout, alloc, dealloc},
    boxed::Box,
};

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr};
use kmod_loader::{KernelModuleHelper, SectionMemOps, SectionPerm};

static RESOLVE_SYMBOL_WARNED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

struct KmodSectionMem {
    ptr: *mut u8,
    size: usize,
    layout: Layout,
}

impl KmodSectionMem {
    fn new(size: usize) -> Option<Self> {
        let aligned_size = (size + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);
        let layout = Layout::from_size_align(aligned_size, PAGE_SIZE_4K).ok()?;
        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            return None;
        }
        unsafe { core::ptr::write_bytes(ptr, 0, aligned_size) };
        Some(Self {
            ptr,
            size: aligned_size,
            layout,
        })
    }
}

impl SectionMemOps for KmodSectionMem {
    fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    fn change_perms(&mut self, perms: SectionPerm) -> bool {
        let vaddr = VirtAddr::from(self.ptr as usize);
        let mut aspace = ax_mm::kernel_aspace().lock();
        let (_, original_flags, _) = match aspace.page_table().query(vaddr) {
            Ok(r) => r,
            Err(_) => return false,
        };
        let mut new_flags = ax_runtime::hal::paging::MappingFlags::empty();
        if perms.contains(SectionPerm::READ) {
            new_flags |= ax_runtime::hal::paging::MappingFlags::READ;
        }
        if perms.contains(SectionPerm::WRITE) {
            new_flags |= ax_runtime::hal::paging::MappingFlags::WRITE;
        }
        if perms.contains(SectionPerm::EXECUTE) {
            new_flags |= ax_runtime::hal::paging::MappingFlags::EXECUTE;
        }
        if aspace.protect(vaddr, self.size, new_flags).is_err() {
            return false;
        }
        for offset in (0..self.size).step_by(PAGE_SIZE_4K) {
            ax_runtime::hal::cpu::asm::flush_tlb(Some(vaddr + offset));
        }
        if perms.contains(SectionPerm::EXECUTE) {
            ax_runtime::hal::cpu::asm::flush_icache_all();
        }
        drop(aspace);
        let _ = original_flags;
        true
    }
}

unsafe impl Send for KmodSectionMem {}
unsafe impl Sync for KmodSectionMem {}

impl Drop for KmodSectionMem {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { dealloc(self.ptr, self.layout) };
        }
    }
}

struct NullSectionMem;

impl SectionMemOps for NullSectionMem {
    fn as_ptr(&self) -> *const u8 {
        core::ptr::null()
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        core::ptr::null_mut()
    }

    fn change_perms(&mut self, _perms: SectionPerm) -> bool {
        false
    }
}

pub struct StarryKmodHelper;

impl KernelModuleHelper for StarryKmodHelper {
    fn vmalloc(size: usize) -> Box<dyn SectionMemOps> {
        match KmodSectionMem::new(size) {
            Some(mem) => Box::new(mem),
            None => {
                warn!("kmod: vmalloc failed for size {size}");
                Box::new(NullSectionMem)
            }
        }
    }

    fn resolve_symbol(_name: &str) -> Option<usize> {
        if RESOLVE_SYMBOL_WARNED
            .compare_exchange(
                false,
                true,
                core::sync::atomic::Ordering::Relaxed,
                core::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
        {
            warn!("kmod: resolve_symbol not yet available (depends on PR #837 kallsyms)");
        }
        None
    }

    fn flsuh_cache(addr: usize, size: usize) {
        #[cfg(target_arch = "aarch64")]
        ax_runtime::hal::cpu::asm::clean_dcache_range_to_pou(VirtAddr::from(addr), size);
        let _ = (addr, size);
        ax_runtime::hal::cpu::asm::flush_icache_all();
    }
}

#[allow(dead_code)]
pub fn load_module_from_memory(
    elf_data: &[u8],
    args: &str,
) -> ax_errno::AxResult<kmod_loader::ModuleOwner<StarryKmodHelper>> {
    let loader = kmod_loader::ModuleLoader::<StarryKmodHelper>::new(elf_data).map_err(|e| {
        warn!("kmod: failed to create loader: {e:?}");
        ax_errno::AxError::Io
    })?;
    let c_args = alloc::ffi::CString::new(args).map_err(|_| ax_errno::AxError::InvalidInput)?;
    loader.load_module(c_args).map_err(|e| {
        warn!("kmod: failed to load module: {e:?}");
        ax_errno::AxError::Io
    })
}

pub fn sys_init_module(
    elf_ptr: usize,
    elf_len: usize,
    args_ptr: usize,
) -> ax_errno::AxResult<isize> {
    if elf_ptr == 0 || elf_len == 0 {
        return Err(ax_errno::AxError::InvalidInput);
    }
    let elf_data = unsafe { core::slice::from_raw_parts(elf_ptr as *const u8, elf_len) };
    let args = if args_ptr != 0 {
        unsafe {
            let p = args_ptr as *const i8;
            let len = (0..).take_while(|&i| *p.add(i) != 0).count();
            core::str::from_utf8(core::slice::from_raw_parts(p as *const u8, len)).unwrap_or("")
        }
    } else {
        ""
    };
    let mut owner = load_module_from_memory(elf_data, args)?;
    owner.call_init().map_err(|e| {
        warn!("kmod: module init failed: {e:?}");
        ax_errno::AxError::Io
    })?;
    info!("kmod: module loaded and initialized successfully");
    // Intentionally leak the module owner: the loaded module must remain in
    // memory for the kernel's lifetime.  When `delete_module` is implemented
    // the owner can be recovered (e.g. via a global registry) and dropped.
    core::mem::forget(owner);
    Ok(0)
}

pub fn sys_delete_module(_name_ptr: usize, _flags: usize) -> ax_errno::AxResult<isize> {
    warn!("kmod: delete_module not yet fully implemented");
    Err(ax_errno::AxError::Unsupported)
}

pub fn sys_finit_module(fd: i32, args_ptr: usize, _flags: usize) -> ax_errno::AxResult<isize> {
    if fd < 0 {
        return Err(ax_errno::AxError::BadFileDescriptor);
    }
    let args = if args_ptr != 0 {
        unsafe {
            let p = args_ptr as *const i8;
            let len = (0..).take_while(|&i| *p.add(i) != 0).count();
            core::str::from_utf8(core::slice::from_raw_parts(p as *const u8, len)).unwrap_or("")
        }
    } else {
        ""
    };
    let _ = (fd, args);
    warn!("kmod: finit_module not yet fully implemented (need file fd to content)");
    Err(ax_errno::AxError::Unsupported)
}
