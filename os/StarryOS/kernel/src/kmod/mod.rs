//! Loadable Kernel Module (LKM) loader and helper glue.
//!
//! Ported from `Starry-OS/StarryOS:ebpf-kmod` (`kernel/src/kmod/`). The
//! source enabled only the `kprint` shim file inline (`#[path =
//! "shim/kprint.rs"]`); the block / mq / xarray shims under
//! `kmod/shim/` were committed but left commented out as in-progress
//! null_blk work. We mirror that scope: this PR ships `kmod/mod.rs` +
//! `kmod/kprint.rs` only. Full block / mq shims are out of scope for the
//! MVP completion criteria (`init_module`/`delete_module`/`finit_module`
//! loading an empty module).
//!
//! Package-name imports adapted to tgoskits (`axhal` → `ax_runtime::hal`,
//! `axalloc` → `ax_alloc`, `axmm` → `ax_mm`, `kspin` → `ax_kspin`) per
//! `crate-fork-audit.md §6`. KALLSYMS lookup goes through the in-kernel
//! `.kallsyms` blob (`crate::pseudofs::proc::KALLSYMS`), the same table
//! `perf::kprobe` resolves names against.

mod kprint;
mod kshim;

use alloc::{
    boxed::Box,
    collections::btree_map::BTreeMap,
    ffi::CString,
    string::{String, ToString},
};

#[cfg(target_arch = "loongarch64")]
use ax_alloc::{UsageKind, global_allocator};
use ax_errno::{AxError, AxResult, LinuxError};
use ax_kspin::SpinNoPreempt;
#[cfg(not(target_arch = "loongarch64"))]
use ax_memory_addr::{MemoryAddr, VirtAddrRange};
use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr};
#[cfg(not(target_arch = "loongarch64"))]
use ax_runtime::hal::paging::MappingFlags;
use kmod_loader::{KernelModuleHelper, ModuleLoader, ModuleOwner, SectionMemOps};

/// Marker type that satisfies `kmod_loader::KernelModuleHelper`. Stateless —
/// every operation reaches into the tgoskits subsystems directly.
pub struct KmodHelper;

#[cfg(not(target_arch = "loongarch64"))]
fn section_perms_to_mapping_flags(perms: kmod_loader::SectionPerm) -> MappingFlags {
    let mut flags = MappingFlags::empty();
    if perms.contains(kmod_loader::SectionPerm::READ) {
        flags |= MappingFlags::READ;
    }
    if perms.contains(kmod_loader::SectionPerm::WRITE) {
        flags |= MappingFlags::WRITE;
    }
    if perms.contains(kmod_loader::SectionPerm::EXECUTE) {
        flags |= MappingFlags::EXECUTE;
    }
    flags
}

/// Owned region of physical frames mapped into the kernel address space.
/// Implements `kmod_loader::SectionMemOps` so the loader can write code /
/// data into the section and later re-protect it.
struct KmodMemSection {
    vaddr: VirtAddr,
    num_pages: usize,
    backend: KmodMemBackend,
}

enum KmodMemBackend {
    #[cfg(not(target_arch = "loongarch64"))]
    KernelAspace,
    #[cfg(target_arch = "loongarch64")]
    DirectMap,
}

impl SectionMemOps for KmodMemSection {
    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.vaddr.as_mut_ptr()
    }

    fn as_ptr(&self) -> *const u8 {
        self.vaddr.as_ptr()
    }

    fn change_perms(&mut self, perms: kmod_loader::SectionPerm) -> bool {
        match self.backend {
            #[cfg(not(target_arch = "loongarch64"))]
            KmodMemBackend::KernelAspace => {
                let mapping_flags = section_perms_to_mapping_flags(perms);
                let kspace = ax_mm::kernel_aspace();
                let mut guard = kspace.lock();
                guard
                    .protect(self.vaddr, PAGE_SIZE_4K * self.num_pages, mapping_flags)
                    .is_ok()
            }
            #[cfg(target_arch = "loongarch64")]
            KmodMemBackend::DirectMap => {
                // LoongArch module text is allocated from the DMW direct-map
                // window so PCALA relocations can reach DMW-linked kernel
                // symbols. DMW translations do not consult the page table, so
                // there are no PTE permissions to update here.
                if perms.contains(kmod_loader::SectionPerm::EXECUTE) {
                    ax_runtime::hal::cache::flush_icache_all();
                }
                true
            }
        }
    }
}

impl Drop for KmodMemSection {
    fn drop(&mut self) {
        match self.backend {
            #[cfg(not(target_arch = "loongarch64"))]
            KmodMemBackend::KernelAspace => {
                let total = PAGE_SIZE_4K * self.num_pages;
                ax_mm::kernel_aspace()
                    .lock()
                    .unmap(self.vaddr, total)
                    .unwrap_or_else(|_| {
                        error!(
                            "kmod: failed to unmap module section at {:#x} ({} pages)",
                            self.vaddr.as_usize(),
                            self.num_pages
                        );
                    });
                crate::mm::flush_tlb_range(self.vaddr, total);
            }
            #[cfg(target_arch = "loongarch64")]
            KmodMemBackend::DirectMap => {
                // SAFETY: KmodMem owns this direct-map allocation and retains
                // the original page count until its single Drop.
                unsafe {
                    global_allocator().deallocate_pages_raw(
                        self.vaddr.as_usize(),
                        ax_alloc::PageRelease {
                            count: self.num_pages,
                            zone: ax_alloc::MemoryZone::Normal,
                        },
                        UsageKind::VirtMem,
                    );
                }
                ax_runtime::hal::cache::flush_icache_all();
            }
        }
    }
}

#[cfg(not(target_arch = "loongarch64"))]
unsafe extern "C" {
    fn _ekernel();
}

#[cfg(not(target_arch = "loongarch64"))]
fn alloc_kmod_frames(num_pages: usize) -> AxResult<VirtAddr> {
    let total = PAGE_SIZE_4K * num_pages;
    let kernel_end = (_ekernel as *const () as usize).align_up_4k();
    // The kernel virtual address space is laid out like this:
    // ┌──────────────────────────────┐
    // │       Free for modules       │
    // ├──────────────────────────────┤
    // │       Kernel text/data       │ high addresses
    // ├──────────────────────────────┤
    let kmod_alloc_start = VirtAddr::from_usize(kernel_end);
    let vaddr = {
        let kspace = ax_mm::kernel_aspace();
        let mut guard = kspace.lock();
        let vaddr = guard
            .find_free_area(
                kmod_alloc_start,
                total,
                VirtAddrRange::new(guard.base(), guard.end()),
            )
            .ok_or(AxError::NoMemory)?;
        guard.map_alloc(vaddr, total, MappingFlags::READ | MappingFlags::WRITE, true)?;
        vaddr
    };
    unsafe { core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, total) };
    Ok(vaddr)
}

#[cfg(target_arch = "loongarch64")]
fn alloc_kmod_dmw_frames(num_pages: usize) -> AxResult<VirtAddr> {
    // LoongArch kernel symbols are exported from the DMW address window
    // (0x9000...). If module sections live in the page-table-backed
    // 0xffff8... kernel space, PCALA relocations against kernel symbols can
    // reconstruct a wrong high-half alias. Allocate physical pages through the
    // global allocator and use the returned direct-map VA so module code and
    // DMW-linked kernel code share the same PC-relative address class.
    let vaddr = VirtAddr::from_usize(
        global_allocator()
            .allocate_pages_raw(
                ax_alloc::PageRequest {
                    count: num_pages,
                    align: PAGE_SIZE_4K,
                    zone: ax_alloc::MemoryZone::Normal,
                },
                UsageKind::VirtMem,
            )
            .map_err(|_| AxError::NoMemory)?,
    );
    unsafe { core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, PAGE_SIZE_4K * num_pages) };
    Ok(vaddr)
}

fn linux_code_to_ax_error(code: i32) -> AxError {
    LinuxError::try_from(code)
        .map(AxError::from)
        .unwrap_or_else(|_| AxError::from(LinuxError::EINVAL))
}

impl KernelModuleHelper for KmodHelper {
    fn vmalloc(size: usize) -> Box<dyn SectionMemOps> {
        assert!(
            size.is_multiple_of(PAGE_SIZE_4K),
            "kmod vmalloc size must be page-aligned"
        );
        let num_pages = size / PAGE_SIZE_4K;
        #[cfg(target_arch = "loongarch64")]
        let (vaddr, backend) = (
            alloc_kmod_dmw_frames(num_pages).expect("kmod vmalloc: out of memory"),
            KmodMemBackend::DirectMap,
        );
        #[cfg(not(target_arch = "loongarch64"))]
        let (vaddr, backend) = (
            alloc_kmod_frames(num_pages).expect("kmod vmalloc: out of memory"),
            KmodMemBackend::KernelAspace,
        );
        Box::new(KmodMemSection {
            vaddr,
            num_pages,
            backend,
        })
    }

    fn resolve_symbol(name: &str) -> Option<usize> {
        if name.is_empty() {
            return None;
        }
        // Resolve against the real in-kernel `.kallsyms` blob (the same table
        // `/proc/kallsyms` is built from), matching `perf::kprobe`'s lookup.
        match crate::pseudofs::proc::KALLSYMS
            .get()
            .and_then(|t| t.lookup_name(name))
        {
            Some(addr) => Some(addr as usize),
            None => {
                error!("kmod: failed to resolve symbol `{}`", name);
                None
            }
        }
    }

    fn flsuh_cache(_addr: usize, _size: usize) {
        // A freshly-relocated module's instructions were just written through
        // the *data* side of the cache hierarchy. On architectures with
        // non-coherent I/D caches (aarch64, riscv64, loongarch64) the CPU may
        // otherwise fetch stale instructions — or fault — from the new code
        // pages, so the instruction cache must be invalidated in addition to
        // the TLB. Mirrors `mm::access::sync_modified_kernel_text`.
        ax_runtime::hal::cache::sync_kernel_text(VirtAddr::from_usize(_addr), _size);
    }
}

type Module = ModuleOwner<KmodHelper>;

/// Registry of currently-loaded modules, keyed by `modinfo` name.
static MODULES: SpinNoPreempt<BTreeMap<String, Module>> = SpinNoPreempt::new(BTreeMap::new());

/// Linux-style `init_module(2)`: take a `.ko` image and an optional
/// parameter string, perform relocations, run the module's `init`
/// function, and register the module in the global table.
pub fn init_module(elf: &[u8], params: Option<&str>) -> AxResult<()> {
    let loader =
        ModuleLoader::<KmodHelper>::new(elf).map_err(|err| linux_code_to_ax_error(err.code()))?;
    let params = match params {
        Some(p) => CString::new(p).map_err(|_| AxError::InvalidInput)?,
        None => CString::new("").unwrap(),
    };
    let mut owner = loader
        .load_module(params)
        .map_err(|err| linux_code_to_ax_error(err.code()))?;

    // `name` is available as soon as `load_module()` returns, before init runs.
    let name = owner.name().to_string();

    // Reject a duplicate name *before* running the module's init function.
    // `call_init()` executes the module's real init (registering callbacks,
    // allocating resources, …) and consumes the init entry, and `ModuleOwner`
    // has no Drop-based exit; running init first and only then bailing out with
    // `EEXIST` would leave those side effects in place with no way to roll
    // them back.
    if MODULES.lock().contains_key(&name) {
        return Err(AxError::AlreadyExists);
    }

    let ret = owner
        .call_init()
        .map_err(|err| linux_code_to_ax_error(err.code()))?;
    if ret != 0 {
        warn!("module `{name}` init returned {ret}");
        return Err(AxError::InvalidInput);
    }
    info!("module `{name}` loaded");

    let mut modules = MODULES.lock();
    // Re-check under the same lock: a concurrent load of the same name may have
    // won the race while this module was running its init. Roll back our init
    // via `call_exit()` rather than leaking it.
    if modules.contains_key(&name) {
        drop(modules);
        owner.call_exit();
        return Err(AxError::AlreadyExists);
    }
    modules.insert(name, owner);
    Ok(())
}

/// Linux-style `delete_module(2)`: look up by `modinfo` name, call the
/// module's `exit`, drop the registration (which deallocates section
/// memory via `KmodMem::drop`).
pub fn delete_module(name: &str) -> AxResult<()> {
    let mut modules = MODULES.lock();
    let mut owner = modules.remove(name).ok_or(AxError::NotFound)?;
    owner.call_exit();
    warn!("module `{name}` exited");
    Ok(())
}

/// `printk`-side init: feed the C `lwprintf` library a callback that
/// emits each character through `ax_print!`, so loaded modules can use
/// `printk` / `pr_info!` and get bytes on the host console.
struct StdOut;
impl lwprintf_rs::CustomOutPut for StdOut {
    fn putch(ch: i32) -> i32 {
        ax_print!("{}", ch as u8 as char);
        ch
    }
}

/// Initialize the LKM subsystem. Must be called at kernel start, before
/// any `sys_init_module(2)` arrives.
pub fn init_kmod() {
    lwprintf_rs::lwprintf_init::<StdOut>();
    ax_println!("kmod subsystem initialized");
}
