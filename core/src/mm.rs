//! User address space management.

use alloc::{
    borrow::ToOwned, collections::btree_map::BTreeMap, string::String, sync::Arc, vec::Vec,
};
use core::ffi::CStr;

use axerrno::{LinuxError, LinuxResult};
use axfs_ng::FS_CONTEXT;
use axhal::{
    mem::virt_to_phys,
    paging::{MappingFlags, PageSize},
};
use axmm::AddrSpace;
use kernel_elf_parser::{ELFParser, app_stack_region};
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use spin::lock_api::RwLock;
use xmas_elf::{ElfFile, program::SegmentData};

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
    let signal_trampoline_paddr = virt_to_phys(axsignal::arch::signal_trampoline_address().into());
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

    for segement in elf_parser.ph_load() {
        debug!(
            "Mapping ELF segment: [{:#x?}, {:#x?}) flags: {}",
            segement.vaddr,
            segement.vaddr + segement.memsz as usize,
            segement.flags
        );
        let seg_pad = segement.vaddr.align_offset_4k();
        assert_eq!(seg_pad, segement.offset % PAGE_SIZE_4K);

        let seg_align_size =
            (segement.memsz as usize + seg_pad + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);
        uspace.map_alloc(
            VirtAddr::from_usize(segement.vaddr).align_down_4k(),
            seg_align_size,
            mapping_flags(segement.flags),
            true,
            PageSize::Size4K,
        )?;
        let seg_data = elf
            .input
            .get(segement.offset..segement.offset + segement.filesz as usize)
            .ok_or(LinuxError::EINVAL)?;
        uspace.write(segement.vaddr.into(), seg_data)?;
        // TDOO: flush the I-cache
    }

    Ok(elf_parser)
}

static CACHED_ELF: RwLock<BTreeMap<usize, Arc<ElfFile>>> = RwLock::new(BTreeMap::new());

/// Inserts an ELF file into the cache.
pub fn insert_elf_cache(path: &str) -> LinuxResult<()> {
    let fs = FS_CONTEXT.lock();
    let loc = fs.resolve(path)?;
    let file_data = fs.read(path)?.leak();
    let elf = ElfFile::new(file_data).map_err(|_| LinuxError::ENOEXEC)?;
    CACHED_ELF
        .write()
        .insert(loc.entry().as_ptr(), Arc::new(elf));
    Ok(())
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
        let new_args: Vec<String> = core::iter::once("/bin/sh".to_owned())
            .chain(args.iter().cloned())
            .collect();
        return load_user_app(uspace, None, &new_args, envs);
    }

    let ptr = FS_CONTEXT.lock().resolve(path)?.entry().as_ptr();
    let elf_owner = match CACHED_ELF.read().get(&ptr) {
        Some(elf) => Ok(elf.clone()),
        None => {
            let file_data = FS_CONTEXT.lock().read(path)?;
            if file_data.starts_with(b"#!") {
                let head = &file_data[2..file_data.len().min(256)];
                let pos = head.iter().position(|c| *c == b'\n').unwrap_or(head.len());
                let line = core::str::from_utf8(&head[..pos]).map_err(|_| LinuxError::EINVAL)?;

                let new_args: Vec<String> = line
                    .trim()
                    .splitn(2, |c: char| c.is_ascii_whitespace())
                    .map(|s| s.trim_ascii().to_owned())
                    .chain(args.iter().cloned())
                    .collect();
                return load_user_app(uspace, None, &new_args, envs);
            }
            Err(file_data)
        }
    };
    let elf_object = match &elf_owner {
        Ok(elf) => Ok(elf.as_ref()),
        Err(file_data) => Err(ElfFile::new(file_data).map_err(|_| LinuxError::ENOEXEC)?),
    };
    let elf = match &elf_object {
        Ok(elf) => *elf,
        Err(elf) => elf,
    };

    let ldso_entry_and_base = if let Some(header) = elf
        .program_iter()
        .find(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Interp))
    {
        let ldso = match header.get_data(elf) {
            Ok(SegmentData::Undefined(data)) => data,
            _ => panic!("Invalid data in Interp Elf Program Header"),
        };
        let ldso = CStr::from_bytes_with_nul(ldso)
            .ok()
            .and_then(|it| it.to_str().ok())
            .ok_or(LinuxError::EINVAL)?;
        debug!("Loading dynamic linker: {}", ldso);
        let mut loader = |elf: &ElfFile| -> LinuxResult<Option<(usize, usize)>> {
            let ldso_parser = map_elf(uspace, crate::config::USER_INTERP_BASE, elf)?;
            Ok(Some((ldso_parser.entry(), ldso_parser.base())))
        };
        let ptr = FS_CONTEXT.lock().resolve(ldso)?.entry().as_ptr();
        match CACHED_ELF.read().get(&ptr) {
            Some(elf) => loader(elf)?,
            None => {
                let file_data = FS_CONTEXT.lock().read(ldso)?;
                let ldso_elf = ElfFile::new(&file_data).map_err(|_| LinuxError::ENOEXEC)?;
                loader(&ldso_elf)?
            }
        }
    } else {
        None
    };

    let elf_parser = map_elf(uspace, uspace.base().as_usize(), elf)?;
    let entry = ldso_entry_and_base
        .map(|it| it.0)
        .unwrap_or_else(|| elf_parser.entry());
    let auxv = elf_parser
        .aux_vector(PAGE_SIZE_4K, ldso_entry_and_base.map(|it| it.1))
        .collect::<Vec<_>>();

    // The user stack is divided into two parts:
    // `ustack_start` -> `ustack_pointer`: It is the stack space that users actually
    // read and write. `ustack_pointer` -> `ustack_end`: It is the space that
    // contains the arguments, environment variables and auxv passed to the app.
    //  When the app starts running, the stack pointer points to `ustack_pointer`.
    let ustack_end = VirtAddr::from_usize(crate::config::USER_STACK_TOP);
    let ustack_size = crate::config::USER_STACK_SIZE;
    let ustack_start = ustack_end - ustack_size;
    debug!(
        "Mapping user stack: {:#x?} -> {:#x?}",
        ustack_start, ustack_end
    );

    let stack_data = app_stack_region(args, envs, &auxv, ustack_end.into());
    uspace.map_alloc(
        ustack_start,
        ustack_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        true,
        PageSize::Size4K,
    )?;

    let user_sp = ustack_end - stack_data.len();
    uspace.write(user_sp, stack_data.as_slice())?;

    let heap_start = VirtAddr::from_usize(crate::config::USER_HEAP_BASE);
    let heap_size = crate::config::USER_HEAP_SIZE;
    uspace.map_alloc(
        heap_start,
        heap_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        true,
        PageSize::Size4K,
    )?;

    Ok((VirtAddr::from(entry), user_sp))
}

#[percpu::def_percpu]
static mut ACCESSING_USER_MEM: bool = false;

/// Enables scoped access into user memory, allowing page faults to occur inside
/// kernel.
pub fn access_user_memory<R>(f: impl FnOnce() -> R) -> R {
    ACCESSING_USER_MEM.with_current(|v| {
        unsafe {
            core::ptr::write_volatile(v, true);
        }
        let result = f();
        unsafe {
            core::ptr::write_volatile(v, false);
        }
        result
    })
}

/// Check if the current thread is accessing user memory.
pub fn is_accessing_user_memory() -> bool {
    ACCESSING_USER_MEM.read_current()
}
