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

use alloc::{
    boxed::Box,
    collections::btree_map::BTreeMap,
    ffi::CString,
    string::{String, ToString},
};

use ax_alloc::{UsageKind, global_allocator};
use ax_errno::{AxError, AxResult, LinuxError};
use ax_kspin::SpinNoPreempt;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr};
use ax_runtime::hal::{
    cpu::asm::{flush_icache_all, flush_tlb},
    mem::{phys_to_virt, virt_to_phys},
    paging::{MappingFlags, PageSize},
};
use kmod_loader::{KernelModuleHelper, ModuleLoader, ModuleOwner, SectionMemOps};

/// Marker type that satisfies `kmod_loader::KernelModuleHelper`. Stateless —
/// every operation reaches into the tgoskits subsystems directly.
pub struct KmodHelper;

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
struct KmodMem {
    paddr: PhysAddr,
    vaddr: VirtAddr,
    num_pages: usize,
}

impl SectionMemOps for KmodMem {
    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.vaddr.as_mut_ptr()
    }

    fn as_ptr(&self) -> *const u8 {
        self.vaddr.as_ptr()
    }

    fn change_perms(&mut self, perms: kmod_loader::SectionPerm) -> bool {
        let mapping_flags = section_perms_to_mapping_flags(perms);
        let kspace = ax_mm::kernel_aspace();
        let mut guard = kspace.lock();
        guard
            .protect(self.vaddr, PAGE_SIZE_4K * self.num_pages, mapping_flags)
            .is_ok()
    }
}

impl Drop for KmodMem {
    fn drop(&mut self) {
        let total = PAGE_SIZE_4K * self.num_pages;

        // While the module was live, `change_perms()` re-protected its sections
        // (e.g. `.text` → RX, `.rodata` → RO) in the kernel address space. The
        // global page allocator only maintains a free-page list — it never
        // touches the kernel PTEs — so handing back still-RO/RX pages would
        // corrupt a later reuse: `alloc_kmod_frames()`'s zeroing `write_bytes()`
        // would fault on a read-only page, or a non-writable page would be
        // handed to some other kernel object. Restore a plain RW mapping (and
        // flush the now-stale TLB entries) before returning the frames.
        let restored = {
            let kspace = ax_mm::kernel_aspace();
            let mut guard = kspace.lock();
            guard
                .protect(self.vaddr, total, MappingFlags::READ | MappingFlags::WRITE)
                .is_ok()
        };
        if !restored {
            // Don't silently return RO/RX pages to the general allocator pool;
            // leak them instead so a later allocation can't fault on them.
            error!(
                "kmod: failed to restore RW mapping for module section at {:#x} ({} pages); \
                 leaking frames",
                self.vaddr.as_usize(),
                self.num_pages
            );
            return;
        }
        crate::mm::flush_tlb_range(self.vaddr, total);

        let vaddr = phys_to_virt(self.paddr);
        global_allocator().dealloc_pages(vaddr.as_usize(), self.num_pages, UsageKind::Global);
    }
}

fn alloc_kmod_frames(num_pages: usize) -> AxResult<(PhysAddr, VirtAddr)> {
    let page_size = PageSize::Size4K as usize;
    let total = page_size * num_pages;
    let vaddr_usize = global_allocator()
        .alloc_pages(num_pages, page_size, UsageKind::Global)
        .map_err(|_| AxError::NoMemory)?;
    let vaddr = VirtAddr::from(vaddr_usize);
    // SAFETY: just-allocated, page-aligned region of `total` bytes.
    unsafe { core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, total) };
    Ok((virt_to_phys(vaddr), vaddr))
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
        let (paddr, vaddr) = alloc_kmod_frames(num_pages).expect("kmod vmalloc: out of memory");
        Box::new(KmodMem {
            paddr,
            vaddr,
            num_pages,
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
        flush_tlb(None);
        flush_icache_all();
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
