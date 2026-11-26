//! User address space management.

use alloc::{borrow::ToOwned, collections::VecDeque, string::String, sync::Arc, vec, vec::Vec};
use core::{
    ffi::CStr,
    iter,
    mem::{align_of, size_of},
};

use ax_fs_ng::{
    FsView,
    vfs::{CachedFile, FileBackend},
};
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use ax_runtime::hal::{mem::virt_to_phys, paging::MappingFlags};
use axfs_ng_vfs::{NodeType, PathHandle};
use kernel_elf_parser::{AuxEntry, AuxType, ELFHeaders, ELFHeadersBuilder, ELFParser};
use ouroboros::self_referencing;
use zerocopy::IntoBytes;

use crate::{
    StarryError, StarryResult,
    mm::{
        UserVirtualAddressLayout,
        aspace::{AddrSpace, AddressSpaceId, MappingOperation, VmEpoch},
    },
    namespace::UserNamespace,
    sync::RawSpinLock,
};

/// Largest argv/envp stack image accepted by execve.
///
/// Linux derives this from the process stack limit and allows argv/envp to use
/// at most one quarter of it. StarryOS has a fixed 8 MiB user stack, so this
/// yields a 2 MiB limit while leaving room for the ELF auxiliary vector and
/// stack alignment.
pub(crate) const MAX_EXEC_ARG_BYTES: usize = crate::config::USER_STACK_SIZE / 4;

/// Reject argv/envp sets that cannot fit within the exec argument budget.
///
/// Count both C-string terminators and the two terminating pointer slots: all
/// of them become part of the initial user stack image.
pub(crate) fn validate_exec_arg_size(args: &[String], envs: &[String]) -> StarryResult {
    let pointer_count = args
        .len()
        .checked_add(envs.len())
        .and_then(|count| count.checked_add(2))
        .ok_or(StarryError::ArgumentListTooLong)?;
    let mut total = pointer_count
        .checked_mul(size_of::<usize>())
        .ok_or(StarryError::ArgumentListTooLong)?;

    for value in args.iter().chain(envs.iter()) {
        total = total
            .checked_add(
                value
                    .len()
                    .checked_add(1)
                    .ok_or(StarryError::ArgumentListTooLong)?,
            )
            .ok_or(StarryError::ArgumentListTooLong)?;
    }

    if total > MAX_EXEC_ARG_BYTES {
        return Err(StarryError::ArgumentListTooLong);
    }
    Ok(())
}

// RISC-V relocation types
#[cfg(target_arch = "riscv64")]
const R_RISCV_RELATIVE: u32 = 3;
#[cfg(target_arch = "riscv64")]
const R_RISCV_JUMP_SLOT: u32 = 5;
#[cfg(target_arch = "riscv64")]
const R_RISCV_64: u32 = 2;
#[cfg(target_arch = "riscv64")]
const R_RISCV_COPY: u32 = 4;

// Linux rejects PT_INTERP paths outside PATH_MAX before allocation.
const MAX_INTERPRETER_PATH_LEN: u64 = 4096;

/// Creates a new empty user address space.
pub fn new_user_aspace_empty() -> StarryResult<AddrSpace> {
    AddrSpace::new_user(UserVirtualAddressLayout::platform_default()?)
}

/// An exec address space that is still private to the loader.
///
/// The scheduler and process lifecycle APIs cannot consume this type.  A
/// successful load must first turn it into [`PreparedUserImage`], mirroring
/// Linux's nascent `bprm->mm` before `begin_new_exec()` installs it.
#[must_use = "an unpublished user image must be loaded or explicitly discarded"]
pub struct UserImageBuilder {
    aspace: AddrSpace,
}

/// Proof that the current contents of one [`UserImageBuilder`] completed all
/// loader steps.  Identity and epoch bind the token to that exact attempt, so
/// a token from an earlier ENOEXEC retry cannot publish a later image.
#[must_use = "a loaded image token must be consumed by UserImageBuilder::finish"]
pub struct LoadedUserImage {
    space_id: AddressSpaceId,
    epoch: VmEpoch,
    entry: VirtAddr,
    stack: VirtAddr,
    auxv: Vec<AuxEntry>,
}

/// A fully loaded image that has not yet crossed exec's point of no return.
#[must_use = "a prepared user image must be installed or explicitly discarded"]
pub struct PreparedUserImage {
    aspace: AddrSpace,
    entry: VirtAddr,
    stack: VirtAddr,
    auxv: Vec<AuxEntry>,
}

impl PreparedUserImage {
    /// Consumes the unpublished typestate immediately before the caller creates
    /// the first [`super::MmHandle`] and installs it in a process transaction.
    pub fn into_parts(self) -> (AddrSpace, VirtAddr, VirtAddr, Vec<AuxEntry>) {
        (self.aspace, self.entry, self.stack, self.auxv)
    }
}

impl UserImageBuilder {
    /// Sets the nascent MM's authority before loading or publishing its image.
    pub(crate) fn set_user_namespace(&mut self, namespace: Arc<RawSpinLock<UserNamespace>>) {
        self.aspace.set_user_namespace(namespace);
    }

    /// Consumes a successfully loaded attempt after verifying that no later
    /// retry changed the builder contents represented by `loaded`.
    pub fn finish(self, loaded: LoadedUserImage) -> StarryResult<PreparedUserImage> {
        if self.aspace.address_space_id() != loaded.space_id
            || self.aspace.vm_epoch() != loaded.epoch
        {
            return Err(StarryError::BadState);
        }
        Ok(PreparedUserImage {
            aspace: self.aspace,
            entry: loaded.entry,
            stack: loaded.stack,
            auxv: loaded.auxv,
        })
    }
}

/// If the target architecture requires it, the kernel portion of the address
/// space will be copied to the user address space.
pub fn copy_from_kernel(_aspace: &mut AddrSpace) -> StarryResult {
    #[cfg(not(any(target_arch = "aarch64", target_arch = "loongarch64")))]
    {
        // ARMv8 (aarch64) and LoongArch64 use separate page tables for user space
        // (aarch64: TTBR0_EL1, LoongArch64: PGDL), so there is no need to copy the
        // kernel portion to the user page table.
        let kspace = ax_mm::kernel_aspace().lock();
        // SAFETY: the global kernel address space outlives every user address
        // space, whose managed regions are restricted to user-space addresses.
        unsafe { _aspace.share_kernel_root_entries_from(kspace.root_entry_share()) }
            .map_err(|_| StarryError::BadState)?;
    }
    Ok(())
}

/// Allocates the nascent address space used by one exec attempt.
pub fn new_user_image_builder() -> StarryResult<UserImageBuilder> {
    let mut aspace = new_user_aspace_empty()?;
    copy_from_kernel(&mut aspace)?;
    Ok(UserImageBuilder { aspace })
}

/// Map the signal trampoline to the user address space.
pub fn map_trampoline(aspace: &mut AddrSpace) -> StarryResult {
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
        mapping_flags |= MappingFlags::WRITE | MappingFlags::READ;
    }
    if flags.is_execute() {
        mapping_flags |= MappingFlags::EXECUTE;
    }
    mapping_flags
}

fn app_stack_region(args: &[String], envs: &[String], auxv: &[AuxEntry], sp: usize) -> Vec<u8> {
    let mut data = VecDeque::new();
    let mut push = |src: &[u8]| -> usize {
        data.extend(src.iter().copied());
        data.rotate_right(src.len());
        sp - data.len()
    };

    let random_str_pos = push(b"0123456789abcdef");
    let envs_slice: Vec<_> = envs
        .iter()
        .map(|env| {
            push(b"\0");
            push(env.as_bytes())
        })
        .collect();
    let argv_slice: Vec<_> = args
        .iter()
        .map(|arg| {
            push(b"\0");
            push(arg.as_bytes())
        })
        .collect();
    let padding_null = "\0".repeat(size_of::<usize>());
    let sp = push(padding_null.as_bytes());

    push(&b"\0".repeat(sp % 16));

    if (envs.len() + args.len() + 3) & 1 != 0 {
        push(padding_null.as_bytes());
    }

    let has_random = auxv.iter().any(|entry| entry.get_type() == AuxType::RANDOM);
    let has_execfn = auxv.iter().any(|entry| entry.get_type() == AuxType::EXECFN);

    // `push` prepends bytes to the stack image. Push the terminator first so
    // user memory presents auxv as: supplied entries, AT_RANDOM, AT_EXECFN,
    // AT_NULL. Without AT_NULL, musl keeps parsing argv/env padding as auxv
    // and can falsely enable AT_SECURE.
    push(AuxEntry::new(AuxType::NULL, 0).as_bytes());
    if !has_execfn {
        push(AuxEntry::new(AuxType::EXECFN, argv_slice[0]).as_bytes());
    }
    if !has_random {
        push(AuxEntry::new(AuxType::RANDOM, random_str_pos).as_bytes());
    }
    push(auxv.as_bytes());

    push(padding_null.as_bytes());
    push(envs_slice.as_bytes());
    push(padding_null.as_bytes());
    push(argv_slice.as_bytes());
    let sp = push(args.len().as_bytes());

    assert!(sp % 16 == 0);

    let mut result = Vec::with_capacity(data.len());
    let (first, second) = data.as_slices();
    result.extend_from_slice(first);
    result.extend_from_slice(second);
    result
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
    entry: &'a ElfImage,
) -> StarryResult<ELFParser<'a>> {
    let elf_parser =
        ELFParser::new(entry.borrow_elf(), base).map_err(|_| StarryError::InvalidData)?;
    let cache = entry.borrow_cache();

    // PT_TLS init image may extend beyond the last PT_LOAD's file range.
    // This assumes the PT_TLS file data is contiguous with and immediately
    // follows the last PT_LOAD segment's file extent, which is the standard
    // layout produced by GNU ld and LLVM lld.
    // Compute the maximum file offset needed so the COW backend can serve
    // TLS init-image page faults for the dynamic linker.
    let tls_max_offset: u64 = elf_parser
        .headers()
        .ph
        .iter()
        .filter(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Tls))
        .map(|ph| {
            debug!(
                "PT_TLS: vaddr={:#x} memsz={:#x} filesz={:#x} offset={:#x}",
                ph.virtual_addr, ph.mem_size, ph.file_size, ph.offset
            );
            ph.offset + ph.file_size
        })
        .max()
        .unwrap_or(0);

    let load_segments: Vec<_> = elf_parser
        .headers()
        .ph
        .iter()
        .filter(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Load))
        .collect();
    let last_load_idx = load_segments.len().wrapping_sub(1);

    for (i, ph) in load_segments.iter().enumerate() {
        let vaddr = ph.virtual_addr as usize + elf_parser.base();
        debug!(
            "Mapping ELF segment: [{:#x?}, {:#x?}) flags: {}",
            vaddr,
            vaddr + ph.mem_size as usize,
            ph.flags
        );
        let seg_pad = vaddr.align_offset_4k();
        // ELF requires each loadable segment's virtual address and file
        // offset to have the same page offset. This is untrusted executable
        // metadata, so reject a mismatch instead of panicking in the kernel.
        if seg_pad != ph.offset as usize % PAGE_SIZE_4K {
            return Err(StarryError::MalformedExecutable);
        }

        let seg_align_size =
            (ph.mem_size as usize + seg_pad + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);
        let seg_start = VirtAddr::from_usize(vaddr);

        // Note that `offset` might not be aligned to 4K here, and it's
        // backend's responsibility to properly handle it.
        let file_end = if i == last_load_idx && tls_max_offset > ph.offset + ph.file_size {
            tls_max_offset
        } else {
            ph.offset + ph.file_size
        };
        let backend = MappingOperation::new_cow(
            seg_start,
            PAGE_SIZE_4K,
            FileBackend::Cached(cache.clone()),
            ph.offset,
            Some(file_end),
            false,
        );
        uspace.map(
            seg_start.align_down_4k(),
            seg_align_size,
            mapping_flags(ph.flags),
            false,
            backend,
        )?;
    }

    // Apply relocations for static-pie binaries
    // On non-riscv64 architectures, apply_relocations() is a no-op stub.
    if elf_parser.headers().header.pt1.class() == xmas_elf::header::Class::SixtyFour {
        let is_pie = elf_parser.headers().header.pt2.type_().as_type()
            == xmas_elf::header::Type::SharedObject;
        if is_pie {
            #[cfg(target_arch = "riscv64")]
            {
                // Populate PT_LOAD segments so relocation writes can access pages
                for seg in elf_parser
                    .headers()
                    .ph
                    .iter()
                    .filter(|p| p.get_type() == Ok(xmas_elf::program::Type::Load))
                {
                    let seg_start =
                        VirtAddr::from_usize(base + seg.virtual_addr as usize).align_down_4k();
                    let seg_pad = (base + seg.virtual_addr as usize).align_offset_4k();
                    let seg_size =
                        (seg.mem_size as usize + seg_pad + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);
                    uspace.populate_area(seg_start, seg_size, mapping_flags(seg.flags))?;
                }
            }
            apply_relocations(uspace, base, entry.borrow_cache(), &elf_parser.headers().ph)?;
        }
    }

    Ok(elf_parser)
}

/// Reproduce Linux v7.1's `load_elf_binary()` data-bound calculation for the
/// main executable. `start_data` is the greatest PT_LOAD start, while
/// `end_data` is the greatest PT_LOAD file end; the interpreter is excluded.
fn executable_data_layout(elf: &ELFParser<'_>) -> StarryResult<(usize, usize)> {
    let mut start_data = 0usize;
    let mut end_data = 0usize;
    let mut found_load = false;
    for header in elf
        .headers()
        .ph
        .iter()
        .filter(|header| header.get_type() == Ok(xmas_elf::program::Type::Load))
    {
        found_load = true;
        let segment_start = elf
            .base()
            .checked_add(
                usize::try_from(header.virtual_addr)
                    .map_err(|_| StarryError::MalformedExecutable)?,
            )
            .ok_or(StarryError::MalformedExecutable)?;
        let file_end = segment_start
            .checked_add(
                usize::try_from(header.file_size).map_err(|_| StarryError::MalformedExecutable)?,
            )
            .ok_or(StarryError::MalformedExecutable)?;
        start_data = start_data.max(segment_start);
        end_data = end_data.max(file_end);
    }
    if !found_load || end_data < start_data {
        return Err(StarryError::MalformedExecutable);
    }
    Ok((start_data, end_data))
}

/// Convert a virtual address to a file offset using PT_LOAD segments.
///
/// This function searches through the program headers to find which PT_LOAD
/// segment contains the given virtual address, then calculates the
/// corresponding file offset.
///
/// Returns None if the address is not within any PT_LOAD segment.
#[cfg(target_arch = "riscv64")]
fn vaddr_to_file_offset(vaddr: u64, ph: &[xmas_elf::program::ProgramHeader64]) -> Option<usize> {
    let vaddr = vaddr as usize;
    for seg in ph {
        if seg.get_type() != Ok(xmas_elf::program::Type::Load) {
            continue;
        }
        let seg_vaddr = seg.virtual_addr as usize;
        let seg_filesz = seg.file_size as usize;
        if vaddr >= seg_vaddr && vaddr < seg_vaddr + seg_filesz {
            let offset_in_segment = vaddr - seg_vaddr;
            return Some(seg.offset as usize + offset_in_segment);
        }
    }
    None
}

/// Apply relocations for static-pie binaries.
///
/// This processes .rela.dyn and .rela.plt sections to apply
/// R_RISCV_RELATIVE and R_RISCV_JUMP_SLOT relocations.
#[cfg(target_arch = "riscv64")]
fn apply_relocations(
    uspace: &mut AddrSpace,
    base: usize,
    cache: &CachedFile,
    ph: &[xmas_elf::program::ProgramHeader64],
) -> StarryResult {
    // Find PT_DYNAMIC segment
    let dynamic_ph = ph
        .iter()
        .find(|p| p.get_type() == Ok(xmas_elf::program::Type::Dynamic));

    let dynamic_ph = match dynamic_ph {
        Some(ph) => ph,
        None => return Ok(()), // No dynamic section, nothing to do
    };

    // Read dynamic entries from file
    let dyn_offset = dynamic_ph.offset as usize;
    let dyn_size = dynamic_ph.file_size as usize;

    if dyn_offset + dyn_size > (cache.location().len().unwrap_or(0) as usize) {
        debug!("Dynamic section extends beyond file");
        return Err(StarryError::InvalidData);
    }

    let mut dyn_data = vec![0u8; dyn_size];
    cache.read_at(&mut dyn_data, dyn_offset as u64)?;
    let entry_size = 16; // sizeof(Dynamic<u64>) = 16 bytes
    let num_entries = dyn_size / entry_size;

    // Parse dynamic entries using byte-by-byte reading
    let mut rela_addr: u64 = 0;
    let mut rela_size: u64 = 0;
    let mut jmprel_addr: u64 = 0;
    let mut jmprel_size: u64 = 0;
    let mut symtab_addr: u64 = 0;
    let mut strtab_addr: u64 = 0;

    for i in 0..num_entries {
        let offset = i * entry_size;
        let entry_data = &dyn_data[offset..offset + entry_size];

        // Dynamic entry: tag (8 bytes) + value (8 bytes)
        let tag = u64::from_le_bytes(entry_data[0..8].try_into().unwrap());
        let value = u64::from_le_bytes(entry_data[8..16].try_into().unwrap());

        match tag {
            7 => rela_addr = value,    // DT_RELA
            8 => rela_size = value,    // DT_RELASZ
            23 => jmprel_addr = value, // DT_JMPREL
            2 => jmprel_size = value,  // DT_PLTRELSZ
            6 => symtab_addr = value,  // DT_SYMTAB
            5 => strtab_addr = value,  // DT_STRTAB
            0 => break,                // DT_NULL
            _ => {}
        }
    }

    // Process .rela.dyn (R_RISCV_RELATIVE)
    if rela_addr != 0 && rela_size != 0 {
        let rela_offset = vaddr_to_file_offset(rela_addr, ph).ok_or(StarryError::InvalidData)?;
        let rela_entry_size = 24; // sizeof(Rela<u64>) = 24 bytes
        let rela_count = rela_size as usize / rela_entry_size;
        let mut copy_count: usize = 0;

        debug!("Processing {} RELATIVE relocations", rela_count);

        for i in 0..rela_count {
            let entry_offset = rela_offset + i * rela_entry_size;
            if entry_offset + rela_entry_size > (cache.location().len().unwrap_or(0) as usize) {
                break;
            }

            let mut entry_data = vec![0u8; rela_entry_size];
            cache.read_at(&mut entry_data, entry_offset as u64)?;

            // Rela entry: offset (8 bytes) + info (8 bytes) + addend (8 bytes)
            let offset = u64::from_le_bytes(entry_data[0..8].try_into().unwrap()) as usize;
            let info = u64::from_le_bytes(entry_data[8..16].try_into().unwrap());
            let addend = i64::from_le_bytes(entry_data[16..24].try_into().unwrap());

            let reloc_type = (info & 0xffffffff) as u32;

            match reloc_type {
                R_RISCV_RELATIVE => {
                    // *(base + offset) = base + addend
                    let target = base + offset;
                    let value = (base as i64 + addend) as u64;
                    uspace.write(VirtAddr::from_usize(target), &value.to_le_bytes())?;
                    debug!("RELATIVE: [{:#x}] = {:#x}", target, value);
                }
                R_RISCV_64 => {
                    // S + A (symbol value + addend)
                    let sym_idx = (info >> 32) as usize;
                    if symtab_addr == 0 || strtab_addr == 0 {
                        debug!("Missing symtab/strtab for R_RISCV_64");
                        continue;
                    }

                    let sym_file_offset =
                        vaddr_to_file_offset(symtab_addr, ph).ok_or(StarryError::InvalidData)?;
                    let sym_entry_offset = sym_file_offset + sym_idx * 24;
                    let file_len = cache.location().len().unwrap_or(0) as usize;
                    if sym_entry_offset + 24 > file_len {
                        continue;
                    }
                    let mut sym_data = vec![0u8; 24];
                    cache.read_at(&mut sym_data, sym_entry_offset as u64)?;
                    let st_value = u64::from_le_bytes(sym_data[8..16].try_into().unwrap());
                    if st_value == 0 {
                        continue;
                    }
                    let target = base + offset;
                    let value = (base as i64 + st_value as i64 + addend) as u64;
                    uspace.write(VirtAddr::from_usize(target), &value.to_le_bytes())?;
                }
                R_RISCV_COPY => {
                    copy_count += 1;
                }
                _ => {
                    debug!("[apply_relocations] unknown .rela.dyn type={}", reloc_type);
                }
            }
        }
        if copy_count > 0 {
            debug!(
                "[apply_relocations] skipped {} R_RISCV_COPY relocations",
                copy_count
            );
        }
    }

    // Process .rela.plt (R_RISCV_JUMP_SLOT)
    if jmprel_addr != 0 && jmprel_size != 0 {
        let jmprel_offset =
            vaddr_to_file_offset(jmprel_addr, ph).ok_or(StarryError::InvalidData)?;
        let rela_entry_size = 24; // sizeof(Rela<u64>) = 24 bytes
        let jmprel_count = jmprel_size as usize / rela_entry_size;

        debug!("Processing {} JUMP_SLOT relocations", jmprel_count);

        for i in 0..jmprel_count {
            let entry_offset = jmprel_offset + i * rela_entry_size;
            if entry_offset + rela_entry_size > (cache.location().len().unwrap_or(0) as usize) {
                break;
            }

            let mut entry_data = vec![0u8; rela_entry_size];
            cache.read_at(&mut entry_data, entry_offset as u64)?;

            // Rela entry: offset (8 bytes) + info (8 bytes) + addend (8 bytes)
            let offset = u64::from_le_bytes(entry_data[0..8].try_into().unwrap()) as usize;
            let info = u64::from_le_bytes(entry_data[8..16].try_into().unwrap());
            let _addend = i64::from_le_bytes(entry_data[16..24].try_into().unwrap());

            let reloc_type = (info & 0xffffffff) as u32;
            let sym_idx = (info >> 32) as usize;

            match reloc_type {
                R_RISCV_JUMP_SLOT => {
                    // For static-pie, symbols are in the binary itself
                    // We need to look up the symbol in .dynsym
                    if symtab_addr == 0 || strtab_addr == 0 {
                        debug!("Missing symtab/strtab for JUMP_SLOT");
                        continue;
                    }

                    // Read symbol from .dynsym
                    let sym_file_offset =
                        vaddr_to_file_offset(symtab_addr, ph).ok_or(StarryError::InvalidData)?;
                    let sym_entry_offset = sym_file_offset + sym_idx * 24;
                    let file_len = cache.location().len().unwrap_or(0) as usize;
                    if sym_entry_offset + 24 > file_len {
                        continue;
                    }
                    let mut sym_data = vec![0u8; 24];
                    cache.read_at(&mut sym_data, sym_entry_offset as u64)?;
                    let st_value = u64::from_le_bytes(sym_data[8..16].try_into().unwrap());

                    if st_value == 0 {
                        continue;
                    }
                    let target = base + offset;
                    let value = base as u64 + st_value;
                    uspace.write(VirtAddr::from_usize(target), &value.to_le_bytes())?;
                }
                _ => {
                    debug!("Unsupported relocation type: {}", reloc_type);
                }
            }
        }
    }

    Ok(())
}

/// Stub for non-riscv64 architectures
#[cfg(not(target_arch = "riscv64"))]
fn apply_relocations(
    _uspace: &mut AddrSpace,
    _base: usize,
    _cache: &CachedFile,
    _ph: &[xmas_elf::program::ProgramHeader64],
) -> StarryResult {
    Ok(())
}

fn map_elf_error(err: &'static str) -> StarryError {
    debug!("Failed to parse ELF file: {err}");
    StarryError::InvalidExecutable
}

#[self_referencing]
/// Parsed headers owned by one load; data pages remain shared by `CachedFile`.
struct ElfImage {
    cache: CachedFile,
    // The header parser borrows native integer fields from this allocation.
    data: Vec<u64>,
    #[borrows(data)]
    #[covariant]
    elf: ELFHeaders<'this>,
}

#[derive(Clone, Copy)]
enum ElfImageKind {
    Main,
    Interpreter,
}

struct NativeElfAbi {
    machine: xmas_elf::header::Machine,
    requires_class64: bool,
}

const NATIVE_ELF_ABI: NativeElfAbi = cfg_select! {
    target_arch = "x86_64" => NativeElfAbi {
        machine: xmas_elf::header::Machine::X86_64,
        requires_class64: false,
    },
    target_arch = "aarch64" => NativeElfAbi {
        machine: xmas_elf::header::Machine::AArch64,
        requires_class64: false,
    },
    target_arch = "riscv64" => NativeElfAbi {
        machine: xmas_elf::header::Machine::RISC_V,
        requires_class64: true,
    },
    target_arch = "loongarch64" => NativeElfAbi {
        machine: xmas_elf::header::Machine::Other(258),
        requires_class64: true,
    },
};

const ELF_CLASS_OFFSET: usize = xmas_elf::header::MAGIC.len();
const ELF_CLASS64: u8 = 2;

// Linux bounds the native program-header table in load_elf_phdrs().
const MAX_PROGRAM_HEADER_BYTES: usize = 65536;

impl ElfImage {
    fn load(loc: PathHandle, kind: ElfImageKind) -> StarryResult<Result<Self, Vec<u8>>> {
        // A retained inode can have new bytes after its previous execution.
        // Read its headers anew rather than treating path identity as content.
        let cache = CachedFile::get_or_create(loc)?;

        let mut data = vec![0_u64; PAGE_SIZE_4K / size_of::<u64>()];
        let read = cache.read_at(bytemuck::cast_slice_mut(&mut data), 0)?;
        let minimum_header_size = match kind {
            ElfImageKind::Main => 0,
            // Linux reads a complete native interpreter header before checking its format.
            ElfImageKind::Interpreter => {
                size_of::<xmas_elf::header::HeaderPt1>()
                    + size_of::<xmas_elf::header::HeaderPt2_<xmas_elf::P64>>()
            }
        };
        if read < minimum_header_size {
            return Err(StarryError::UnexpectedEof);
        }
        let bytes = &mut bytemuck::cast_slice_mut::<_, u8>(&mut data)[..read];
        let original_class = if bytes.starts_with(&xmas_elf::header::MAGIC) {
            bytes.get_mut(ELF_CLASS_OFFSET).map(|class| {
                let original = *class;
                // Linux's native loader selects the layout from its ABI, not
                // the class byte. Only this load's private header is changed.
                *class = ELF_CLASS64;
                original
            })
        } else {
            None
        };
        match ElfImage::try_new_or_recover::<StarryError>(cache.clone(), data, |data| {
            if NATIVE_ELF_ABI.requires_class64 && original_class != Some(ELF_CLASS64) {
                return Err(StarryError::InvalidExecutable);
            }
            let data = &bytemuck::cast_slice(data)[..read];
            Self::parse_headers(&cache, data, kind)
        }) {
            Ok(e) => Ok(Ok(e)),
            Err((_, heads)) => {
                let mut bytes = bytemuck::cast_slice(&heads.data)[..read].to_vec();
                if let Some(original) = original_class
                    && let Some(class) = bytes.get_mut(ELF_CLASS_OFFSET)
                {
                    *class = original;
                }
                Ok(Err(bytes))
            }
        }
    }

    fn parse_headers<'a>(
        cache: &CachedFile,
        data: &'a [u8],
        kind: ElfImageKind,
    ) -> StarryResult<ELFHeaders<'a>> {
        // Check the native ABI before following program-header offsets.
        let header = xmas_elf::header::parse_header(data).map_err(map_elf_error)?;
        if header.pt2.machine().as_machine() != NATIVE_ELF_ABI.machine
            || (matches!(kind, ElfImageKind::Main)
                && !matches!(
                    header.pt2.type_().as_type(),
                    xmas_elf::header::Type::Executable | xmas_elf::header::Type::SharedObject
                ))
        {
            return Err(StarryError::InvalidExecutable);
        }

        let entry_size = size_of::<xmas_elf::program::ProgramHeader64>();
        if header.pt2.ph_entry_size() as usize != entry_size {
            return Err(StarryError::InvalidExecutable);
        }
        let table_size = (header.pt2.ph_count() as usize)
            .checked_mul(entry_size)
            .filter(|size| (1..=MAX_PROGRAM_HEADER_BYTES).contains(size))
            .ok_or(StarryError::InvalidExecutable)?;
        let start = header.pt2.ph_offset();
        let end = start
            .checked_add(table_size as u64)
            .ok_or(StarryError::InvalidExecutable)?;
        let builder = ELFHeadersBuilder::new(data).map_err(map_elf_error)?;
        if end <= data.len() as u64 {
            let table = &data[start as usize..end as usize];
            if table
                .as_ptr()
                .addr()
                .is_multiple_of(align_of::<xmas_elf::program::ProgramHeader64>())
            {
                return builder.build(table).map_err(map_elf_error);
            }
        }

        // File offsets may be unaligned. The builder copies its entries, so
        // this bounded, aligned staging allocation need not outlive the call.
        let mut words = vec![0_u64; table_size / size_of::<u64>()];
        let table = bytemuck::cast_slice_mut(&mut words);
        if end <= data.len() as u64 {
            table.copy_from_slice(&data[start as usize..end as usize]);
        } else if cache.read_at(&mut *table, start)? != table_size {
            return Err(StarryError::InvalidExecutable);
        }
        builder.build(table).map_err(map_elf_error)
    }
}

type LoadResult = Result<(VirtAddr, Vec<AuxEntry>), Vec<u8>>;

fn load_elf(
    uspace: &mut AddrSpace,
    loc: PathHandle,
    view: &FsView<'_>,
) -> StarryResult<LoadResult> {
    // A main input can be a short shebang script rather than an ELF image.
    let elf = match ElfImage::load(loc, ElfImageKind::Main)? {
        Ok(elf) => elf,
        Err(data) => return Ok(Err(data)),
    };

    uspace.reset_uninstalled_for_loader()?;
    map_trampoline(uspace)?;

    let entry = &elf;
    let ldso = if let Some(header) = entry
        .borrow_elf()
        .ph
        .iter()
        .find(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Interp))
    {
        let cache = entry.borrow_cache();
        let interp_len = header.file_size;
        let interp_end = header
            .offset
            .checked_add(interp_len)
            .ok_or(StarryError::MalformedExecutable)?;
        if !(2..=MAX_INTERPRETER_PATH_LEN).contains(&interp_len) || interp_end > cache.len() {
            return Err(StarryError::MalformedExecutable);
        }

        let mut data = vec![0; interp_len as usize];
        let read = cache.read_at(&mut data[..], header.offset)?;
        if read != data.len() {
            return Err(StarryError::MalformedExecutable);
        }

        let ldso = CStr::from_bytes_with_nul(&data)
            .ok()
            .and_then(|cstr| cstr.to_str().ok())
            .ok_or(StarryError::MalformedExecutable)?;
        debug!("Loading dynamic linker: {ldso}");
        Some(ldso.to_owned())
    } else {
        None
    };

    // PT_INTERP is protected while loading, but is not mm->exe_file.
    let _interpreter_access;
    let ldso = if let Some(ldso) = ldso {
        let loc = view.resolve(ldso)?;
        check_executable_access(&loc, view.credentials())?;
        _interpreter_access = ax_fs_ng::file::ExecutableFile::acquire(loc.location().clone())?;
        Some(
            ElfImage::load(loc, ElfImageKind::Interpreter)?
                .map_err(|_| StarryError::InvalidElfInterpreter)?,
        )
    } else {
        None
    };

    let elf = map_elf(uspace, uspace.base().as_usize(), &elf)?;
    let (start_data, end_data) = executable_data_layout(&elf)?;
    uspace.set_executable_data_layout(start_data, end_data)?;
    let ldso = if ldso.is_some() {
        let max_end = uspace
            .max_mapped_end()
            .map(VirtAddr::as_usize)
            .unwrap_or_else(|| uspace.base().as_usize());
        let interp_base = (max_end + 0x100000 - 1) & !(0x100000 - 1);
        ldso.as_ref()
            .map(|elf| map_elf(uspace, interp_base, elf))
            .transpose()?
    } else {
        None
    };

    let entry = VirtAddr::from_usize(
        ldso.as_ref()
            .map_or_else(|| elf.entry(), |ldso| ldso.entry()),
    );
    let has_ldso = ldso.is_some();
    let mut auxv = elf
        .aux_vector(PAGE_SIZE_4K, ldso.map(|elf| elf.base()))
        .collect::<Vec<_>>();
    auxv.push(AuxEntry::new(
        AuxType::HWCAP,
        crate::cpu_capabilities::elf_hwcap(),
    ));
    auxv.push(AuxEntry::new(AuxType::UID, 0));
    auxv.push(AuxEntry::new(AuxType::EUID, 0));
    auxv.push(AuxEntry::new(AuxType::GID, 0));
    auxv.push(AuxEntry::new(AuxType::EGID, 0));
    auxv.push(AuxEntry::new(AuxType::SECURE, 0));

    debug!(
        "loader: entry={:#x} auxv_len={} has_ldso={} auxv_last_type={}",
        entry.as_usize(),
        auxv.len(),
        has_ldso,
        auxv.last()
            .map(|e| e.get_type() as usize)
            .unwrap_or(usize::MAX),
    );

    Ok(Ok((entry, auxv)))
}

// Linux's exec path bounds chained binary-format rewrites and returns ELOOP
// for a too-deep interpreter chain. Give StarryOS's recursive script loader
// the same bounded failure behavior.
const MAX_INTERPRETER_RECURSION: usize = 5;

/// Load the user app to the user address space.
///
/// The executable is identified by an already-resolved [`PathHandle`] — the
/// caller resolves and opens it once (mirroring Linux's `do_open_execat`,
/// which honors `AT_SYMLINK_NOFOLLOW` at that single lookup), and this never
/// re-resolves the main executable from its pathname. Interpreters reached
/// through a `#!` shebang are resolved here by path, which
/// is Linux's `open_exec(interp)` and legitimately follows symlinks.
///
/// # Arguments
/// - `uspace`: The address space of the user app.
/// - `loc`: The resolved executable to load.
/// - `path`: The pathname the executable was invoked as, used for the script
///   name an interpreter receives in `argv`.
/// - `args`: The arguments of the user app.
/// - `envs`: The environment variables of the user app.
/// - `view`: The frozen paths and identity used to resolve the main executable;
///   every interpreter reuses it without reading the current task scope.
///
/// # Returns
/// - The entry point of the user app.
/// - The stack pointer of the user app.
pub fn load_user_app(
    builder: &mut UserImageBuilder,
    loc: PathHandle,
    path: &str,
    args: &[String],
    envs: &[String],
    view: &FsView<'_>,
) -> StarryResult<LoadedUserImage> {
    let result = validate_exec_arg_size(args, envs).and_then(|()| {
        load_user_app_with_depth(&mut builder.aspace, loc, path, args, envs, view, 0)
    });
    match result {
        Ok((entry, stack, auxv)) => Ok(LoadedUserImage {
            space_id: builder.aspace.address_space_id(),
            epoch: builder.aspace.vm_epoch(),
            entry,
            stack,
            auxv,
        }),
        Err(load_error) => {
            // This is an explicit, fallible abort in process context.  Drop is
            // intentionally not responsible for backend/page-table cleanup.
            // If cleanup itself fails, return that stronger ownership error so
            // the caller cannot reuse a partially cleared builder as ENOEXEC.
            match builder.aspace.reset_uninstalled_for_loader() {
                Ok(()) => Err(load_error),
                Err(abort_error) => {
                    warn!(
                        "failed to abort unpublished user image after load error {load_error}: \
                         {abort_error}"
                    );
                    Err(abort_error)
                }
            }
        }
    }
}

/// Checks each executable and interpreter before reading its current image.
fn check_executable_access(
    loc: &PathHandle,
    credentials: &axfs_ng_vfs::MutationCredentials<'_>,
) -> StarryResult<()> {
    let metadata = loc.location().metadata()?;
    if metadata.node_type != NodeType::RegularFile
        || loc.location().mountpoint().mount_flags() & linux_raw_sys::general::MS_NOEXEC != 0
    {
        return Err(StarryError::PermissionDenied);
    }
    let mode = metadata.mode.bits();
    let selected = if credentials.fsuid == metadata.uid {
        mode >> 6
    } else if credentials.fsgid == metadata.gid
        || credentials.supplementary_gids.contains(&metadata.gid)
    {
        mode >> 3
    } else {
        mode
    };
    // CAP_DAC_OVERRIDE may bypass DAC only if some execute bit is set.
    if selected & 1 != 0 || (mode & 0o111 != 0 && credentials.cap_dac_override) {
        Ok(())
    } else {
        Err(StarryError::PermissionDenied)
    }
}

fn load_user_app_with_depth(
    uspace: &mut AddrSpace,
    loc: PathHandle,
    path: &str,
    args: &[String],
    envs: &[String],
    view: &FsView<'_>,
    interpreter_depth: usize,
) -> StarryResult<(VirtAddr, VirtAddr, Vec<AuxEntry>)> {
    check_executable_access(&loc, view.credentials())?;
    let executable_file = ax_fs_ng::file::ExecutableFile::acquire(loc.location().clone())?;

    let (entry, auxv) = match load_elf(uspace, loc, view)? {
        Ok((entry, auxv)) => (entry, auxv),
        Err(data) => {
            if data.starts_with(b"#!") {
                if interpreter_depth >= MAX_INTERPRETER_RECURSION {
                    return Err(StarryError::FilesystemLoop);
                }
                let head = &data[2..data.len().min(256)];
                let pos = head.iter().position(|c| *c == b'\n').unwrap_or(head.len());
                let line =
                    core::str::from_utf8(&head[..pos]).map_err(|_| StarryError::InvalidInput)?;

                let new_args: Vec<String> = line
                    .trim()
                    .splitn(2, |c: char| c.is_ascii_whitespace())
                    .map(|s| s.trim_ascii().to_owned())
                    .chain(iter::once(path.to_owned()))
                    .chain(args.iter().skip(1).cloned())
                    .collect();
                // Open the interpreter by path (Linux's `open_exec` on the
                // shebang interpreter) and load it as the new executable.
                let interp = view.resolve(&new_args[0])?;
                // A script stops being the executable when its interpreter takes over.
                drop(executable_file);
                return load_user_app_with_depth(
                    uspace,
                    interp,
                    &new_args[0],
                    &new_args,
                    envs,
                    view,
                    interpreter_depth + 1,
                );
            }
            return Err(StarryError::InvalidExecutable);
        }
    };

    uspace.set_executable_file(executable_file);

    let ustack_top = uspace.stack_top();
    let ustack_size = crate::config::USER_STACK_SIZE;
    let ustack_start = ustack_top - ustack_size;
    debug!("Mapping user stack: {ustack_start:#x?} -> {ustack_top:#x?}");

    uspace.map(
        ustack_start,
        ustack_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        false,
        MappingOperation::new_alloc(ustack_start, PAGE_SIZE_4K, "[stack]"),
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
        MappingOperation::new_alloc(heap_start, PAGE_SIZE_4K, "[heap]"),
    )?;

    Ok((entry, user_sp, auxv))
}

#[cfg(all(test, axtest))]
mod tests {
    use ax_fs_ng::{FsContext, MemoryFs, NativeLinkCaller, file::OpenOptions};
    use axfs_ng_vfs::Mountpoint;

    use super::*;

    #[axtest::axtest]
    fn script_interpreter_uses_the_supplied_view_and_releases_failed_exec_access() {
        const SCRIPT: &str = "/__loader_view_script";
        const INTERPRETER: &str = "/__loader_view_interpreter";
        let caller = NativeLinkCaller::root();
        let filesystem = MemoryFs::new();
        let mount = Mountpoint::new_root(&filesystem);
        let root = mount.root_location();
        let view = FsView::native(FsContext::from_tree(mount, root).unwrap(), &caller);
        for (name, mode, contents) in [
            (SCRIPT, 0o755, b"#!/__loader_view_interpreter\n".as_slice()),
            (INTERPRETER, 0, b"not executable".as_slice()),
        ] {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&view, name)
                .unwrap()
                .into_file()
                .unwrap();
            assert_eq!(file.write_at(contents, 0).unwrap(), contents.len());
        }
        let mut builder = new_user_image_builder().unwrap();
        let error = load_user_app(
            &mut builder,
            view.resolve(SCRIPT).unwrap(),
            SCRIPT,
            &[SCRIPT.into()],
            &[],
            &view,
        )
        .err();
        assert!(
            matches!(error, Some(StarryError::PermissionDenied)),
            "the private interpreter must be found and checked in the supplied view: {error:?}"
        );
        let file = OpenOptions::new()
            .write(true)
            .open(&view, SCRIPT)
            .unwrap()
            .into_file()
            .unwrap();
        assert_eq!(
            file.write_at(b"reusable".as_slice(), 0).unwrap(),
            8,
            "a failed image must release its executable lease before returning"
        );
    }
}
