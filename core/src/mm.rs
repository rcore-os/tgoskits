//! User address space management.

use alloc::{borrow::ToOwned, string::String, vec::Vec};
use core::{ffi::CStr, iter, mem::MaybeUninit};

use axerrno::{LinuxError, LinuxResult};
use axfs_ng::{FS_CONTEXT, OpenOptions};
use axfs_ng_vfs::{DirEntry, Location};
use axhal::{
    mem::virt_to_phys,
    paging::{MappingFlags, PageSize},
};
use axio::Read;
use axmm::{AddrSpace, Backend};
use axsync::{Mutex, RawMutex};
use axtask::current;
use extern_trait::extern_trait;
use kernel_elf_parser::{AuxEntry, ELFParser, app_stack_region};
use lock_api::ArcMutexGuard;
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use ouroboros::self_referencing;
use starry_vm::{VmError, VmIo, VmResult};
use uluru::LRUCache;
use xmas_elf::{ElfFile, program::SegmentData};

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
    elf: &'a ElfFile,
) -> LinuxResult<ELFParser<'a>> {
    let elf_parser = ELFParser::new(elf, base).map_err(|_| LinuxError::EINVAL)?;

    for segment in elf_parser.ph_load() {
        debug!(
            "Mapping ELF segment: [{:#x?}, {:#x?}) flags: {}",
            segment.vaddr,
            segment.vaddr + segment.memsz as usize,
            segment.flags
        );
        let seg_pad = segment.vaddr.align_offset_4k();
        assert_eq!(seg_pad, segment.offset % PAGE_SIZE_4K);

        let seg_align_size =
            (segment.memsz as usize + seg_pad + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);
        let seg_start = VirtAddr::from_usize(segment.vaddr).align_down_4k();
        uspace.map(
            seg_start,
            seg_align_size,
            mapping_flags(segment.flags),
            true,
            Backend::new_alloc(seg_start, PageSize::Size4K),
        )?;

        let seg_data = elf
            .input
            .get(segment.offset..segment.offset + segment.filesz as usize)
            .ok_or(LinuxError::EINVAL)?;
        uspace.write(segment.vaddr.into(), seg_data)?;
        // TDOO: flush the I-cache
    }

    Ok(elf_parser)
}

#[self_referencing]
struct ElfCacheEntry {
    ent: DirEntry<RawMutex>,
    data: Vec<u8>,
    #[borrows(data)]
    #[covariant]
    elf: ElfFile<'this>,
}

impl ElfCacheEntry {
    fn load(loc: Location<RawMutex>) -> LinuxResult<Result<Self, Vec<u8>>> {
        let ent = loc.entry().clone();
        let mut data = Vec::new();
        OpenOptions::new()
            .read(true)
            .open_loc(loc)?
            .into_file()?
            .read_to_end(&mut data)?;
        match ElfCacheEntry::try_new_or_recover(ent, data, |data| ElfFile::new(data)) {
            Ok(e) => Ok(Ok(e)),
            Err((_, heads)) => Ok(Err(heads.data)),
        }
    }
}

struct ElfLoader(LRUCache<ElfCacheEntry, 16>);

type LoadResult = Result<(VirtAddr, Vec<AuxEntry>), Vec<u8>>;

impl ElfLoader {
    const fn new() -> Self {
        Self(LRUCache::new())
    }

    fn load(&mut self, uspace: &mut AddrSpace, path: &str) -> LinuxResult<LoadResult> {
        let loc = FS_CONTEXT.lock().resolve(path)?;

        if !self.0.touch(|e| e.borrow_ent().ptr_eq(loc.entry())) {
            match ElfCacheEntry::load(loc)? {
                Ok(e) => {
                    self.0.insert(e);
                }
                Err(data) => {
                    return Ok(Err(data));
                }
            }
        }

        let elf = self.0.front().unwrap().borrow_elf();
        let ldso = if let Some(header) = elf
            .program_iter()
            .find(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Interp))
        {
            let Ok(SegmentData::Undefined(ldso)) = header.get_data(elf) else {
                debug!("Invalid data in Interp elf program header");
                return Err(LinuxError::EINVAL);
            };
            let ldso = CStr::from_bytes_with_nul(ldso)
                .ok()
                .and_then(|cstr| cstr.to_str().ok())
                .ok_or(LinuxError::EINVAL)?;
            debug!("Loading dynamic linker: {}", ldso);
            Some(ldso)
        } else {
            None
        };

        let (elf, ldso) = if let Some(ldso) = ldso {
            let loc = FS_CONTEXT.lock().resolve(ldso)?;
            if !self.0.touch(|e| e.borrow_ent().ptr_eq(loc.entry())) {
                let e = ElfCacheEntry::load(loc)?.map_err(|_| LinuxError::EINVAL)?;
                self.0.insert(e);
            }

            let mut iter = self.0.iter();
            let ldso = iter.next().unwrap().borrow_elf();
            let elf = iter.next().unwrap().borrow_elf();
            (elf, Some(ldso))
        } else {
            (elf, None)
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

    let (entry, auxv) = match ELF_LOADER.lock().load(uspace, path)? {
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
        true,
        Backend::new_alloc(ustack_start, PageSize::Size4K),
    )?;

    let stack_data = app_stack_region(args, envs, &auxv, ustack_top.into());
    let user_sp = ustack_top - stack_data.len();
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
        self.0
            .can_access_range(start, len, flags)
            .then_some(())
            .ok_or(VmError::AccessDenied)?;

        let page_start = start.align_down_4k();
        let page_end = (start + len).align_up_4k();
        self.0
            .populate_area(page_start, page_end - page_start, flags)
            .map_err(|_| VmError::AccessDenied)?;

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
