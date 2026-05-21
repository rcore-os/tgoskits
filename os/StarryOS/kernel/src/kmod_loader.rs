use alloc::{
    alloc::{Layout, alloc, dealloc},
    boxed::Box,
};

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr};
use kmod_loader::{KernelModuleHelper, SectionMemOps, SectionPerm};

#[allow(dead_code)]
struct KmodSectionMem {
    ptr: *mut u8,
    size: usize,
    layout: Layout,
}

#[allow(dead_code)]
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
        let mut new_flags = ax_hal::paging::MappingFlags::empty();
        if perms.contains(SectionPerm::READ) {
            new_flags |= ax_hal::paging::MappingFlags::READ;
        }
        if perms.contains(SectionPerm::WRITE) {
            new_flags |= ax_hal::paging::MappingFlags::WRITE;
        }
        if perms.contains(SectionPerm::EXECUTE) {
            new_flags |= ax_hal::paging::MappingFlags::EXECUTE;
        }
        if aspace.protect(vaddr, self.size, new_flags).is_err() {
            return false;
        }
        for offset in (0..self.size).step_by(PAGE_SIZE_4K) {
            ax_hal::asm::flush_tlb(Some(vaddr + offset));
        }
        if perms.contains(SectionPerm::EXECUTE) {
            ax_hal::asm::flush_icache_all();
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
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

#[allow(dead_code)]
pub struct StarryKmodHelper;

impl KernelModuleHelper for StarryKmodHelper {
    fn vmalloc(size: usize) -> Box<dyn SectionMemOps> {
        match KmodSectionMem::new(size) {
            Some(mem) => Box::new(mem),
            None => {
                warn!("kmod: vmalloc failed for size {size}");
                Box::new(KmodSectionMem {
                    ptr: core::ptr::null_mut(),
                    size: 0,
                    layout: Layout::from_size_align(1, 1).unwrap(),
                })
            }
        }
    }

    fn resolve_symbol(name: &str) -> Option<usize> {
        crate::kallsyms::kallsyms_lookup_name(name).map(|addr| addr as usize)
    }

    fn flsuh_cache(addr: usize, size: usize) {
        #[cfg(target_arch = "aarch64")]
        ax_hal::asm::clean_dcache_range_to_pou(VirtAddr::from(addr), size);
        let _ = (addr, size);
        ax_hal::asm::flush_icache_all();
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
