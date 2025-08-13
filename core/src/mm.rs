//! User address space management.

use alloc::{borrow::ToOwned, string::String, vec, vec::Vec};
use core::{ffi::CStr, iter, mem::MaybeUninit};

use axerrno::{LinuxError, LinuxResult};
use axfs_ng::{CachedFile, FS_CONTEXT, FileBackend};
use axfs_ng_vfs::Location;
use axhal::{
    mem::virt_to_phys,
    paging::{MappingFlags, PageSize},
};
use axmm::{AddrSpace, backend::Backend};
use axsync::{Mutex, RawMutex};
use axtask::current;
use extern_trait::extern_trait;
use kernel_elf_parser::{AuxEntry, ELFHeaders, ELFHeadersBuilder, ELFParser, app_stack_region};
use lock_api::ArcMutexGuard;
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use ouroboros::self_referencing;
use starry_vm::{VmError, VmIo, VmResult};
use uluru::LRUCache;

use crate::task::AsThread;

/// Creates a new empty user address space.
pub fn new_user_aspace_empty() -> LinuxResult<AddrSpace> {
    AddrSpace::new_empty(
        VirtAddr::from_usize(crate::config::USER_SPACE_BASE),
        crate::config::USER_SPACE_SIZE,
    )
}

/// If the target architecture requires it, the kernel portion of the address
/// space will be copied to the user address space.
pub fn copy_from_kernel(_aspace: &mut AddrSpace) -> LinuxResult {
    #[cfg(not(any(target_arch = "aarch64", target_arch = "loongarch64")))]
    {
        // ARMv8 (aarch64) and LoongArch64 use separate page tables for user space
        // (aarch64: TTBR0_EL1, LoongArch64: PGDL), so there is no need to copy the
        // kernel portion to the user page table.
        _aspace.copy_mappings_from(&axmm::kernel_aspace().lock())?;
    }
    Ok(())
}

/// Map the signal trampoline to the user address space.
pub fn map_trampoline(aspace: &mut AddrSpace) -> LinuxResult {
    let signal_trampoline_paddr =
        virt_to_phys(starry_signal::arch::signal_trampoline_address().into());
    aspace.map_linear(
        crate::config::SIGNAL_TRAMPOLINE.into(),
        signal_trampoline_paddr,
        PAGE_SIZE_4K,
        MappingFlags::READ | MappingFlags::EXECUTE | MappingFlags::USER,
    )?;
    Ok(())
}

fn mapping_flags(flags: xmas_elf::program::Flags) -> MappingFlags {
    let mut mapping_flags = MappingFlags::USER;
    if flags.is_read() {
        mapping_flags |= MappingFlags::READ;
    }
    if flags.is_write() {
        mapping_flags |= MappingFlags::WRITE;
    }
    if flags.is_execute() {
        mapping_flags |= MappingFlags::EXECUTE;
    }
    mapping_flags
}

/// Map the elf file to the user address space.
///
/// # Arguments
/// - `uspace`: The address space of the user app.
/// - `elf`: The elf file.
///
/// # Returns
/// - The entry point of the user app.
fn map_elf<'a>(
    uspace: &mut AddrSpace,
    base: usize,
    entry: &'a ElfCacheEntry,
) -> LinuxResult<ELFParser<'a>> {
    let elf_parser = ELFParser::new(entry.borrow_elf(), base).map_err(|_| LinuxError::EINVAL)?;
    let cache = entry.borrow_cache();

    for ph in elf_parser
        .headers()
        .ph
        .iter()
        .filter(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Load))
    {
        let vaddr = ph.virtual_addr as usize + elf_parser.base();
        debug!(
            "Mapping ELF segment: [{:#x?}, {:#x?}) flags: {}",
            vaddr,
            vaddr + ph.mem_size as usize,
            ph.flags
        );
        let seg_pad = vaddr.align_offset_4k();
        assert_eq!(seg_pad, ph.offset as usize % PAGE_SIZE_4K);

        let seg_align_size =
            (ph.mem_size as usize + seg_pad + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);
        let seg_start = VirtAddr::from_usize(vaddr);

        // Note that `offset` might not be aligned to 4K here, and it's
        // backend's responsibility to properly handle it.
        let backend = Backend::new_cow(
            seg_start,
            PageSize::Size4K,
            FileBackend::Cached(cache.clone()),
            ph.offset,
            Some(ph.offset + ph.file_size),
        );
        uspace.map(
            seg_start.align_down_4k(),
            seg_align_size,
            mapping_flags(ph.flags),
            false,
            backend,
        )?;

        // TDOO: flush the I-cache
    }

    Ok(elf_parser)
}

fn map_elf_error(err: &'static str) -> LinuxError {
    debug!("Failed to parse ELF file: {err}");
    LinuxError::ENOEXEC
}

#[self_referencing]
struct ElfCacheEntry {
    cache: CachedFile,
    data: Vec<u8>,
    #[borrows(data)]
    #[covariant]
    elf: ELFHeaders<'this>,
}

impl ElfCacheEntry {
    fn load(loc: Location) -> LinuxResult<Result<Self, Vec<u8>>> {
        let cache = CachedFile::get_or_create(loc);

        let mut data = vec![0; 4096];
        let read = cache.read_at(&mut data, 0)?;
        data.truncate(read);
        match ElfCacheEntry::try_new_or_recover::<LinuxError>(cache.clone(), data, |data| {
            let builder = ELFHeadersBuilder::new(data).map_err(map_elf_error)?;
            let range = builder.ph_range();
            if range.end as usize <= data.len() {
                builder.build(&data[range.start as usize..range.end as usize])
            } else {
                let mut buf = vec![0; (range.end - range.start) as usize];
                cache.read_at(&mut buf, range.start)?;
                builder.build(&buf)
            }
            .map_err(map_elf_error)
        }) {
            Ok(e) => Ok(Ok(e)),
            Err((_, heads)) => Ok(Err(heads.data)),
        }
    }
}

struct ElfLoader(LRUCache<ElfCacheEntry, 32>);

type LoadResult = Result<(VirtAddr, Vec<AuxEntry>), Vec<u8>>;

impl ElfLoader {
    const fn new() -> Self {
        Self(LRUCache::new())
    }

    fn load(&mut self, uspace: &mut AddrSpace, path: &str) -> LinuxResult<LoadResult> {
        let loc = FS_CONTEXT.lock().resolve(path)?;

        if !self.0.touch(|e| e.borrow_cache().location().ptr_eq(&loc)) {
            match ElfCacheEntry::load(loc)? {
                Ok(e) => {
                    self.0.insert(e);
                }
                Err(data) => {
                    return Ok(Err(data));
                }
            }
        }

        uspace.clear();
        map_trampoline(uspace)?;

        let entry = self.0.front().unwrap();
        let ldso = if let Some(header) = entry
            .borrow_elf()
            .ph
            .iter()
            .find(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Interp))
        {
            let cache = entry.borrow_cache();
            let mut data = vec![0; header.file_size as usize];
            let read = cache.read_at(&mut data, header.offset)?;
            assert_eq!(data.len(), read);

            let ldso = CStr::from_bytes_with_nul(&data)
                .ok()
                .and_then(|cstr| cstr.to_str().ok())
                .ok_or(LinuxError::EINVAL)?;
            debug!("Loading dynamic linker: {}", ldso);
            Some(ldso.to_owned())
        } else {
            None
        };

        let (elf, ldso) = if let Some(ldso) = ldso {
            let loc = FS_CONTEXT.lock().resolve(ldso)?;
            if !self.0.touch(|e| e.borrow_cache().location().ptr_eq(&loc)) {
                let e = ElfCacheEntry::load(loc)?.map_err(|_| LinuxError::EINVAL)?;
                self.0.insert(e);
            }

            let mut iter = self.0.iter();
            let ldso = iter.next().unwrap();
            let elf = iter.next().unwrap();
            (elf, Some(ldso))
        } else {
            (entry, None)
        };

        let elf = map_elf(uspace, crate::config::USER_SPACE_BASE, elf)?;
        let ldso = ldso
            .map(|elf| map_elf(uspace, crate::config::USER_INTERP_BASE, elf))
            .transpose()?;

        let entry = VirtAddr::from_usize(
            ldso.as_ref()
                .map_or_else(|| elf.entry(), |ldso| ldso.entry()),
        );
        let auxv = elf
            .aux_vector(PAGE_SIZE_4K, ldso.map(|elf| elf.base()))
            .collect::<Vec<_>>();

        Ok(Ok((entry, auxv)))
    }
}

static ELF_LOADER: Mutex<ElfLoader> = Mutex::new(ElfLoader::new());

/// Clear the ELF cache.
///
/// Useful for removing noises during memory leak detect.
pub fn clear_elf_cache() {
    ELF_LOADER.lock().0.clear();
}

/// Load the user app to the user address space.
///
/// # Arguments
/// - `uspace`: The address space of the user app.
/// - `args`: The arguments of the user app. The first argument is the path of
///   the user app.
/// - `envs`: The environment variables of the user app.
///
/// # Returns
/// - The entry point of the user app.
/// - The stack pointer of the user app.
pub fn load_user_app(
    uspace: &mut AddrSpace,
    path: Option<&str>,
    args: &[String],
    envs: &[String],
) -> LinuxResult<(VirtAddr, VirtAddr)> {
    let path = path
        .or_else(|| args.first().map(String::as_str))
        .ok_or(LinuxError::EINVAL)?;

    // FIXME: impl `/proc/self/exe` to let busybox retry running
    if path.ends_with(".sh") {
        let new_args: Vec<String> = iter::once("/bin/sh".to_owned())
            .chain(args.iter().cloned())
            .collect();
        return load_user_app(uspace, None, &new_args, envs);
    }

    let (entry, auxv) = match { ELF_LOADER.lock().load(uspace, path)? } {
        Ok((entry, auxv)) => (entry, auxv),
        Err(data) => {
            if data.starts_with(b"#!") {
                let head = &data[2..data.len().min(256)];
                let pos = head.iter().position(|c| *c == b'\n').unwrap_or(head.len());
                let line = core::str::from_utf8(&head[..pos]).map_err(|_| LinuxError::EINVAL)?;

                let new_args: Vec<String> = line
                    .trim()
                    .splitn(2, |c: char| c.is_ascii_whitespace())
                    .map(|s| s.trim_ascii().to_owned())
                    .chain(iter::once(path.to_owned()))
                    .chain(args.iter().skip(1).cloned())
                    .collect();
                return load_user_app(uspace, None, &new_args, envs);
            }
            return Err(LinuxError::ENOEXEC);
        }
    };

    let ustack_top = VirtAddr::from_usize(crate::config::USER_STACK_TOP);
    let ustack_size = crate::config::USER_STACK_SIZE;
    let ustack_start = ustack_top - ustack_size;
    debug!(
        "Mapping user stack: {:#x?} -> {:#x?}",
        ustack_start, ustack_top
    );

    uspace.map(
        ustack_start,
        ustack_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        false,
        Backend::new_alloc(ustack_start, PageSize::Size4K),
    )?;

    let stack_data = app_stack_region(args, envs, &auxv, ustack_top.into());
    let user_sp = ustack_top - stack_data.len();
    let user_sp_aligned = user_sp.align_down_4k();
    uspace.populate_area(
        user_sp_aligned,
        (ustack_top - user_sp_aligned).align_up_4k(),
        MappingFlags::READ | MappingFlags::WRITE,
    )?;
    uspace.write(user_sp, stack_data.as_slice())?;

    let heap_start = VirtAddr::from_usize(crate::config::USER_HEAP_BASE);
    let heap_size = crate::config::USER_HEAP_SIZE;
    uspace.map(
        heap_start,
        heap_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        true,
        Backend::new_alloc(heap_start, PageSize::Size4K),
    )?;

    Ok((entry, user_sp))
}

struct Vm(ArcMutexGuard<RawMutex, AddrSpace>);

impl Vm {
    fn check(&mut self, start: usize, len: usize, flags: MappingFlags) -> VmResult {
        let start = VirtAddr::from_usize(start);
        if !self.0.can_access_range(start, len, flags) {
            return Err(VmError::AccessDenied);
        }

        let page_start = start.align_down_4k();
        let page_end = (start + len).align_up_4k();
        if self
            .0
            .populate_area(page_start, page_end - page_start, flags)
            .is_err()
        {
            return Err(VmError::AccessDenied);
        }

        Ok(())
    }
}

#[extern_trait]
unsafe impl VmIo for Vm {
    fn new() -> Self {
        Self(current().as_thread().proc_data.aspace.lock_arc())
    }

    fn read(&mut self, start: usize, buf: &mut [MaybeUninit<u8>]) -> VmResult {
        self.check(start, buf.len(), MappingFlags::READ | MappingFlags::USER)?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                start as *const MaybeUninit<u8>,
                buf.as_mut_ptr(),
                buf.len(),
            );
        }
        Ok(())
    }

    fn write(&mut self, start: usize, buf: &[u8]) -> VmResult {
        self.check(start, buf.len(), MappingFlags::WRITE | MappingFlags::USER)?;
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), start as *mut u8, buf.len());
        }
        Ok(())
    }
}
