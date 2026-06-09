extern crate alloc;

use alloc::vec::Vec;
use core::{mem, ptr::NonNull};

use uefi::{
    boot::{self, AllocateType},
    mem::memory_map::MemoryType,
};

use crate::http::{self, KernelLoadError};

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LSB: u8 = 1;
const ELF_VERSION_CURRENT: u8 = 1;
const ELF_MACHINE_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const UEFI_PAGE_SIZE: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfLoadError {
    Download(KernelLoadError),
    TooSmall,
    BadMagic,
    UnsupportedClass,
    UnsupportedEndian,
    UnsupportedVersion,
    UnsupportedMachine,
    HeaderRange,
    ProgramHeaderRange,
    LoadSegmentMissing,
    SegmentRange,
    SegmentAddressOverflow,
    SegmentNotPageAligned,
    AllocateFailed,
    EntryNotInLoadSegment,
    UnsupportedEntrySymbol,
}

#[derive(Debug)]
pub struct LoadedElf {
    pub entry_point: u64,
    pub load_addr: u64,
    pub load_end: u64,
    pub page_count: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Header {
    ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64ProgramHeader {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

#[derive(Debug, Clone, Copy)]
struct LoadSegment {
    offset: u64,
    vaddr: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
}

pub fn download_and_load(
    url: &str,
    expected_size: u64,
    entry_symbol: Option<&str>,
) -> Result<LoadedElf, ElfLoadError> {
    let image = http::download_sized_body(url, expected_size).map_err(ElfLoadError::Download)?;
    load_elf(&image, entry_symbol)
}

fn load_elf(image: &[u8], entry_symbol: Option<&str>) -> Result<LoadedElf, ElfLoadError> {
    let header = read_struct::<Elf64Header>(image, 0).ok_or(ElfLoadError::TooSmall)?;
    validate_header(header)?;
    let segments = load_segments(image, header)?;
    if segments.is_empty() {
        return Err(ElfLoadError::LoadSegmentMissing);
    }

    let load_addr = align_down(
        segments
            .iter()
            .map(|segment| segment.paddr)
            .min()
            .ok_or(ElfLoadError::LoadSegmentMissing)?,
        UEFI_PAGE_SIZE,
    );
    let load_end = align_up(
        segments
            .iter()
            .map(|segment| segment.paddr.checked_add(segment.memsz))
            .collect::<Option<Vec<_>>>()
            .ok_or(ElfLoadError::SegmentAddressOverflow)?
            .into_iter()
            .max()
            .ok_or(ElfLoadError::LoadSegmentMissing)?,
        UEFI_PAGE_SIZE,
    )
    .ok_or(ElfLoadError::SegmentAddressOverflow)?;
    let page_count = usize::try_from((load_end - load_addr) / UEFI_PAGE_SIZE)
        .map_err(|_| ElfLoadError::SegmentAddressOverflow)?;
    let target = boot::allocate_pages(
        AllocateType::Address(load_addr),
        MemoryType::LOADER_DATA,
        page_count,
    )
    .map_err(|_| ElfLoadError::AllocateFailed)?;

    if let Err(err) = copy_segments(image, &segments, load_addr, target) {
        unsafe {
            let _ = boot::free_pages(target, page_count);
        }
        return Err(err);
    }

    let entry = match entry_symbol {
        Some("httpboot_entry") => find_symbol(image, header, "httpboot_entry")
            .and_then(|symbol| virtual_to_physical(symbol, &segments))
            .ok_or(ElfLoadError::EntryNotInLoadSegment)?,
        Some(_) => return Err(ElfLoadError::UnsupportedEntrySymbol),
        None => virtual_to_physical(header.e_entry, &segments)
            .ok_or(ElfLoadError::EntryNotInLoadSegment)?,
    };

    Ok(LoadedElf {
        entry_point: entry,
        load_addr,
        load_end,
        page_count,
    })
}

fn validate_header(header: &Elf64Header) -> Result<(), ElfLoadError> {
    if &header.ident[..4] != ELF_MAGIC {
        return Err(ElfLoadError::BadMagic);
    }
    if header.ident[4] != ELF_CLASS_64 {
        return Err(ElfLoadError::UnsupportedClass);
    }
    if header.ident[5] != ELF_DATA_LSB {
        return Err(ElfLoadError::UnsupportedEndian);
    }
    if header.ident[6] != ELF_VERSION_CURRENT || header.e_version != 1 {
        return Err(ElfLoadError::UnsupportedVersion);
    }
    if header.e_machine != ELF_MACHINE_X86_64 {
        return Err(ElfLoadError::UnsupportedMachine);
    }
    Ok(())
}

fn load_segments(image: &[u8], header: &Elf64Header) -> Result<Vec<LoadSegment>, ElfLoadError> {
    if usize::from(header.e_phentsize) != mem::size_of::<Elf64ProgramHeader>() {
        return Err(ElfLoadError::ProgramHeaderRange);
    }
    let phoff = usize::try_from(header.e_phoff).map_err(|_| ElfLoadError::HeaderRange)?;
    let phnum = usize::from(header.e_phnum);
    let phentsize = usize::from(header.e_phentsize);
    let ph_size = phnum
        .checked_mul(phentsize)
        .ok_or(ElfLoadError::ProgramHeaderRange)?;
    if phoff
        .checked_add(ph_size)
        .ok_or(ElfLoadError::ProgramHeaderRange)?
        > image.len()
    {
        return Err(ElfLoadError::ProgramHeaderRange);
    }

    let mut segments = Vec::new();
    for index in 0..phnum {
        let offset = phoff + index * phentsize;
        let ph = read_struct::<Elf64ProgramHeader>(image, offset)
            .ok_or(ElfLoadError::ProgramHeaderRange)?;
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        if ph.p_filesz > ph.p_memsz {
            return Err(ElfLoadError::SegmentRange);
        }
        if ph.p_paddr % UEFI_PAGE_SIZE != 0 {
            return Err(ElfLoadError::SegmentNotPageAligned);
        }
        let file_start = usize::try_from(ph.p_offset).map_err(|_| ElfLoadError::SegmentRange)?;
        let file_size = usize::try_from(ph.p_filesz).map_err(|_| ElfLoadError::SegmentRange)?;
        if file_start
            .checked_add(file_size)
            .ok_or(ElfLoadError::SegmentRange)?
            > image.len()
        {
            return Err(ElfLoadError::SegmentRange);
        }
        segments.push(LoadSegment {
            offset: ph.p_offset,
            vaddr: ph.p_vaddr,
            paddr: ph.p_paddr,
            filesz: ph.p_filesz,
            memsz: ph.p_memsz,
        });
    }

    segments.sort_by_key(|segment| segment.paddr);
    Ok(segments)
}

fn copy_segments(
    image: &[u8],
    segments: &[LoadSegment],
    load_addr: u64,
    target: NonNull<u8>,
) -> Result<(), ElfLoadError> {
    let base = target.as_ptr();
    for segment in segments {
        let dst_offset = usize::try_from(
            segment
                .paddr
                .checked_sub(load_addr)
                .ok_or(ElfLoadError::SegmentAddressOverflow)?,
        )
        .map_err(|_| ElfLoadError::SegmentAddressOverflow)?;
        let dst = unsafe { base.add(dst_offset) };
        let file_start = usize::try_from(segment.offset).map_err(|_| ElfLoadError::SegmentRange)?;
        let file_size = usize::try_from(segment.filesz).map_err(|_| ElfLoadError::SegmentRange)?;
        if file_size != 0 {
            let file_end = file_start
                .checked_add(file_size)
                .ok_or(ElfLoadError::SegmentRange)?;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    image[file_start..file_end].as_ptr(),
                    dst,
                    file_size,
                );
            }
        }
        let mem_size = usize::try_from(segment.memsz).map_err(|_| ElfLoadError::SegmentRange)?;
        if mem_size > file_size {
            unsafe {
                core::ptr::write_bytes(dst.add(file_size), 0, mem_size - file_size);
            }
        }
    }
    Ok(())
}

fn virtual_to_physical(vaddr: u64, segments: &[LoadSegment]) -> Option<u64> {
    for segment in segments {
        let end = segment.vaddr.checked_add(segment.memsz)?;
        if vaddr >= segment.vaddr && vaddr < end {
            return segment.paddr.checked_add(vaddr.checked_sub(segment.vaddr)?);
        }
    }
    None
}

fn find_symbol(image: &[u8], header: &Elf64Header, name: &str) -> Option<u64> {
    let section_count = usize::from(header.e_shnum);
    let section_size = usize::from(header.e_shentsize);
    if section_size == 0 || section_size < 64 {
        return None;
    }
    let section_offset = usize::try_from(header.e_shoff).ok()?;
    let total = section_count.checked_mul(section_size)?;
    if section_offset.checked_add(total)? > image.len() {
        return None;
    }

    for index in 0..section_count {
        let section = read_section_header(image, section_offset + index * section_size)?;
        if section.sh_type != 2 && section.sh_type != 11 {
            continue;
        }
        let strtab = read_section_header(
            image,
            section_offset + usize::try_from(section.sh_link).ok()? * section_size,
        )?;
        let symbols = section_bytes(image, section.sh_offset, section.sh_size)?;
        let strings = section_bytes(image, strtab.sh_offset, strtab.sh_size)?;
        let entry_size = if section.sh_entsize == 0 {
            24
        } else {
            section.sh_entsize
        };
        for symbol_offset in (0..symbols.len()).step_by(usize::try_from(entry_size).ok()?) {
            let symbol = read_symbol(symbols, symbol_offset)?;
            let symbol_name = cstr_at(strings, usize::try_from(symbol.st_name).ok()?)?;
            if symbol_name == name {
                return Some(symbol.st_value);
            }
        }
    }
    None
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64SectionHeader {
    sh_name: u32,
    sh_type: u32,
    sh_flags: u64,
    sh_addr: u64,
    sh_offset: u64,
    sh_size: u64,
    sh_link: u32,
    sh_info: u32,
    sh_addralign: u64,
    sh_entsize: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Symbol {
    st_name: u32,
    st_info: u8,
    st_other: u8,
    st_shndx: u16,
    st_value: u64,
    st_size: u64,
}

fn read_section_header(image: &[u8], offset: usize) -> Option<Elf64SectionHeader> {
    read_struct::<Elf64SectionHeader>(image, offset).copied()
}

fn read_symbol(image: &[u8], offset: usize) -> Option<&Elf64Symbol> {
    read_struct::<Elf64Symbol>(image, offset)
}

fn section_bytes(image: &[u8], offset: u64, size: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let len = usize::try_from(size).ok()?;
    image.get(start..start.checked_add(len)?)
}

fn cstr_at(bytes: &[u8], offset: usize) -> Option<&str> {
    let tail = bytes.get(offset..)?;
    let len = tail.iter().position(|byte| *byte == 0)?;
    core::str::from_utf8(&tail[..len]).ok()
}

fn read_struct<T>(image: &[u8], offset: usize) -> Option<&T> {
    let end = offset.checked_add(mem::size_of::<T>())?;
    let bytes = image.get(offset..end)?;
    let ptr = bytes.as_ptr();
    if !(ptr as usize).is_multiple_of(mem::align_of::<T>()) {
        return None;
    }
    Some(unsafe { &*(ptr.cast::<T>()) })
}

fn align_down(value: u64, align: u64) -> u64 {
    value / align * align
}

fn align_up(value: u64, align: u64) -> Option<u64> {
    value
        .checked_add(align - 1)
        .map(|value| align_down(value, align))
}
