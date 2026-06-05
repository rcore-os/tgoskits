//! User address space management.

use alloc::{borrow::ToOwned, string::String, vec, vec::Vec};
use core::{ffi::CStr, iter};

use ax_errno::{AxError, AxResult};
use ax_fs::{CachedFile, FS_CONTEXT, FileBackend};
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use ax_runtime::hal::{
    mem::virt_to_phys,
    paging::{MappingFlags, PageSize},
};
use ax_sync::Mutex;
use axfs_ng_vfs::Location;
use kernel_elf_parser::{
    AuxEntry, AuxType, ELFHeaders, ELFHeadersBuilder, ELFParser, app_stack_region,
};
use ouroboros::self_referencing;
use uluru::LRUCache;

use crate::{
    config::{USER_SPACE_BASE, USER_SPACE_SIZE},
    mm::aspace::{AddrSpace, Backend},
};

#[cfg(target_arch = "riscv64")]
const RISCV_COMPAT_HWCAP_IMAFDC: usize = (1 << (b'I' - b'A'))
    | (1 << (b'M' - b'A'))
    | (1 << (b'A' - b'A'))
    | (1 << (b'F' - b'A'))
    | (1 << (b'D' - b'A'))
    | (1 << (b'C' - b'A'));

// RISC-V relocation types
#[cfg(target_arch = "riscv64")]
const R_RISCV_RELATIVE: u32 = 3;
#[cfg(target_arch = "riscv64")]
const R_RISCV_JUMP_SLOT: u32 = 5;
#[cfg(target_arch = "riscv64")]
const R_RISCV_64: u32 = 2;
#[cfg(target_arch = "riscv64")]
const R_RISCV_COPY: u32 = 4;

/// Creates a new empty user address space.
pub fn new_user_aspace_empty() -> AxResult<AddrSpace> {
    AddrSpace::new_empty(VirtAddr::from_usize(USER_SPACE_BASE), USER_SPACE_SIZE)
}

/// If the target architecture requires it, the kernel portion of the address
/// space will be copied to the user address space.
pub fn copy_from_kernel(_aspace: &mut AddrSpace) -> AxResult {
    #[cfg(not(any(target_arch = "aarch64", target_arch = "loongarch64")))]
    {
        // ARMv8 (aarch64) and LoongArch64 use separate page tables for user space
        // (aarch64: TTBR0_EL1, LoongArch64: PGDL), so there is no need to copy the
        // kernel portion to the user page table.
        let kspace = ax_mm::kernel_aspace().lock();
        _aspace.page_table_mut().cursor().copy_from(
            kspace.page_table(),
            kspace.base(),
            kspace.size(),
        );
    }
    Ok(())
}

/// Map the signal trampoline to the user address space.
pub fn map_trampoline(aspace: &mut AddrSpace) -> AxResult {
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
) -> AxResult<ELFParser<'a>> {
    let elf_parser = ELFParser::new(entry.borrow_elf(), base).map_err(|_| AxError::InvalidData)?;
    let cache = entry.borrow_cache();

    // PT_TLS init image may extend beyond the last PT_LOAD's file range.
    // Compute the maximum file offset needed so the COW backend can serve
    // TLS init-image page faults for the dynamic linker.
    let tls_max_offset: u64 = elf_parser
        .headers()
        .ph
        .iter()
        .filter(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Tls))
        .map(|ph| {
            info!(
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
        assert_eq!(seg_pad, ph.offset as usize % PAGE_SIZE_4K);

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
        let backend = Backend::new_cow(
            seg_start,
            PageSize::Size4K,
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

        // TDOO: flush the I-cache
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
) -> AxResult {
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
        return Err(AxError::InvalidData);
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
        let rela_offset = vaddr_to_file_offset(rela_addr, ph).ok_or(AxError::InvalidData)?;
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
                        vaddr_to_file_offset(symtab_addr, ph).ok_or(AxError::InvalidData)?;
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
        let jmprel_offset = vaddr_to_file_offset(jmprel_addr, ph).ok_or(AxError::InvalidData)?;
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
                        vaddr_to_file_offset(symtab_addr, ph).ok_or(AxError::InvalidData)?;
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
) -> AxResult {
    Ok(())
}

fn map_elf_error(err: &'static str) -> AxError {
    debug!("Failed to parse ELF file: {err}");
    AxError::InvalidExecutable
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
    fn load(loc: Location) -> AxResult<Result<Self, Vec<u8>>> {
        let cache = CachedFile::get_or_create(loc);

        let mut data = vec![0; 4096];
        let read = cache.read_at(&mut data[..], 0)?;
        data.truncate(read);
        match ElfCacheEntry::try_new_or_recover::<AxError>(cache.clone(), data, |data| {
            let builder = ELFHeadersBuilder::new(data).map_err(map_elf_error)?;
            let range = builder.ph_range();
            if range.end as usize <= data.len() {
                builder.build(&data[range.start as usize..range.end as usize])
            } else {
                let mut buf = vec![0; (range.end - range.start) as usize];
                cache.read_at(&mut buf[..], range.start)?;
                builder.build(&buf)
            }
            .map_err(map_elf_error)
        }) {
            Ok(e) => Ok(Ok(e)),
            Err((_, heads)) => Ok(Err(heads.data)),
        }
    }
}

/// The value reported in the `AT_HWCAP` auxiliary vector entry.
///
/// `AT_HWCAP` (auxv type 16) advertises architecture-dependent CPU capability
/// bits to userspace. `getauxval(AT_HWCAP)` reads it, and feature-dispatching
/// runtimes gate optional instruction sets on it.
///
/// Per-arch policy:
/// - **loongarch64**: report the baseline the kernel actually provides. The
///   platform enables LSX (128-bit vectors) and LASX (256-bit vectors) at boot
///   via `EUEN.SXE`/`EUEN.ASXE`, and the task/signal save paths preserve all 256
///   vector bits. Therefore we set `CPUCFG | LAM | UAL | FPU | LSX | LASX`.
///   This matters for feature-dispatching libraries such as OpenSSL and numpy.
/// - **riscv64**: report the baseline ISA bits expected by Linux-compatible
///   user space (`IMAFDC`).
/// - **x86_64 / aarch64**: 0. x86 uses CPUID; aarch64 ASIMD/NEON is mandatory.
const fn hwcap_value() -> usize {
    #[cfg(target_arch = "loongarch64")]
    {
        // Linux loongarch HWCAP bits (uapi/asm/hwcap.h):
        const HWCAP_LOONGARCH_CPUCFG: usize = 1 << 0;
        const HWCAP_LOONGARCH_LAM: usize = 1 << 1;
        const HWCAP_LOONGARCH_UAL: usize = 1 << 2;
        const HWCAP_LOONGARCH_FPU: usize = 1 << 3;
        const HWCAP_LOONGARCH_LSX: usize = 1 << 4;
        const HWCAP_LOONGARCH_LASX: usize = 1 << 5;
        HWCAP_LOONGARCH_CPUCFG
            | HWCAP_LOONGARCH_LAM
            | HWCAP_LOONGARCH_UAL
            | HWCAP_LOONGARCH_FPU
            | HWCAP_LOONGARCH_LSX
            | HWCAP_LOONGARCH_LASX
    }
    #[cfg(target_arch = "riscv64")]
    {
        RISCV_COMPAT_HWCAP_IMAFDC
    }
    #[cfg(not(any(target_arch = "loongarch64", target_arch = "riscv64")))]
    {
        0
    }
}

struct ElfLoader(LRUCache<ElfCacheEntry, 32>);

type LoadResult = Result<(VirtAddr, Vec<AuxEntry>), Vec<u8>>;

impl ElfLoader {
    const fn new() -> Self {
        Self(LRUCache::new())
    }

    fn load(&mut self, uspace: &mut AddrSpace, loc: Location) -> AxResult<LoadResult> {
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
            let read = cache.read_at(&mut data[..], header.offset)?;
            assert_eq!(data.len(), read);

            let ldso = CStr::from_bytes_with_nul(&data)
                .ok()
                .and_then(|cstr| cstr.to_str().ok())
                .ok_or(AxError::InvalidInput)?;
            debug!("Loading dynamic linker: {ldso}");
            Some(ldso.to_owned())
        } else {
            None
        };

        let (elf, ldso) = if let Some(ldso) = ldso {
            let loc = FS_CONTEXT.lock().resolve(ldso)?;
            if !self.0.touch(|e| e.borrow_cache().location().ptr_eq(&loc)) {
                let e = ElfCacheEntry::load(loc)?.map_err(|_| AxError::InvalidInput)?;
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
        let ldso = if ldso.is_some() {
            let max_end = uspace
                .areas()
                .map(|area| area.end().as_usize())
                .max()
                .unwrap_or(crate::config::USER_SPACE_BASE);
            let interp_base = (max_end + 0x100000 - 1) & !(0x100000 - 1);
            ldso.map(|elf| map_elf(uspace, interp_base, elf))
                .transpose()?
        } else {
            None
        };

        let entry = VirtAddr::from_usize(
            ldso.as_ref()
                .map_or_else(|| elf.entry(), |ldso| ldso.entry()),
        );
        let mut auxv = elf
            .aux_vector(PAGE_SIZE_4K, ldso.map(|elf| elf.base()))
            .collect::<Vec<_>>();
        // `aux_vector()` only emits PHDR/PHENT/PHNUM/PAGESZ/ENTRY (+BASE). Add
        // AT_HWCAP so `getauxval(AT_HWCAP)` returns the CPU capability bits the
        // kernel actually provides (notably LSX on loongarch64, which numpy
        // requires to import). See `hwcap_value()` for the per-arch policy.
        auxv.push(AuxEntry::new(AuxType::HWCAP, hwcap_value()));

        Ok(Ok((entry, auxv)))
    }
}

static ELF_LOADER: Mutex<ElfLoader> = Mutex::new(ElfLoader::new());

/// Clear the ELF cache.
///
/// Useful for removing noises during memory leak detect.
#[cfg(feature = "memtrack")]
pub fn clear_elf_cache() {
    ELF_LOADER.lock().0.clear();
}

/// Load the user app to the user address space.
///
/// The executable is identified by an already-resolved [`Location`] — the
/// caller resolves and opens it once (mirroring Linux's `do_open_execat`,
/// which honors `AT_SYMLINK_NOFOLLOW` at that single lookup), and this never
/// re-resolves the main executable from its pathname. Interpreters reached
/// through a `.sh` redirect or a `#!` shebang are resolved here by path, which
/// is Linux's `open_exec(interp)` and legitimately follows symlinks.
///
/// # Arguments
/// - `uspace`: The address space of the user app.
/// - `loc`: The resolved executable to load.
/// - `path`: The pathname the executable was invoked as, used for the `.sh`
///   redirect and for the script name an interpreter receives in `argv`.
/// - `args`: The arguments of the user app.
/// - `envs`: The environment variables of the user app.
///
/// # Returns
/// - The entry point of the user app.
/// - The stack pointer of the user app.
pub fn load_user_app(
    uspace: &mut AddrSpace,
    loc: Location,
    path: &str,
    args: &[String],
    envs: &[String],
) -> AxResult<(VirtAddr, VirtAddr, Vec<AuxEntry>)> {
    // `/proc/self/exe` is available in procfs; busybox can `readlink` it
    // to re-exec itself as a shell on ENOEXEC, provided the busybox build
    // includes that fallback (Alpine's prebuilt binary may not).
    if path.ends_with(".sh") {
        let new_args: Vec<String> = iter::once("/bin/sh".to_owned())
            .chain(args.iter().cloned())
            .collect();
        let sh = FS_CONTEXT.lock().resolve("/bin/sh")?;
        return load_user_app(uspace, sh, "/bin/sh", &new_args, envs);
    }

    let (entry, auxv) = match { ELF_LOADER.lock().load(uspace, loc)? } {
        Ok((entry, auxv)) => (entry, auxv),
        Err(data) => {
            if data.starts_with(b"#!") {
                let head = &data[2..data.len().min(256)];
                let pos = head.iter().position(|c| *c == b'\n').unwrap_or(head.len());
                let line = core::str::from_utf8(&head[..pos]).map_err(|_| AxError::InvalidInput)?;

                let new_args: Vec<String> = line
                    .trim()
                    .splitn(2, |c: char| c.is_ascii_whitespace())
                    .map(|s| s.trim_ascii().to_owned())
                    .chain(iter::once(path.to_owned()))
                    .chain(args.iter().skip(1).cloned())
                    .collect();
                // Open the interpreter by path (Linux's `open_exec` on the
                // shebang interpreter) and load it as the new executable.
                let interp = FS_CONTEXT.lock().resolve(&new_args[0])?;
                return load_user_app(uspace, interp, &new_args[0], &new_args, envs);
            }
            return Err(AxError::InvalidExecutable);
        }
    };

    let ustack_top = VirtAddr::from_usize(crate::config::USER_STACK_TOP);
    let ustack_size = crate::config::USER_STACK_SIZE;
    let ustack_start = ustack_top - ustack_size;
    debug!("Mapping user stack: {ustack_start:#x?} -> {ustack_top:#x?}");

    uspace.map(
        ustack_start,
        ustack_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        false,
        Backend::new_alloc(ustack_start, PageSize::Size4K, "[stack]"),
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
        Backend::new_alloc(heap_start, PageSize::Size4K, "[heap]"),
    )?;

    Ok((entry, user_sp, auxv))
}
