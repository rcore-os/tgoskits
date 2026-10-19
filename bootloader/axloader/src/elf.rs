//! Pure ELF64 validation shared by the loader frontends.

extern crate alloc;

use alloc::vec::Vec;

use thiserror::Error;

use crate::target::{TargetSpec, ranges_overlap};

/// The executable flag from an ELF program header.
pub const ELF_PF_X: u32 = 1;
/// Both EFI frontends place ELF images on four-KiB pages.
pub const EFI_PAGE_SIZE: u64 = 4096;

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LSB: u8 = 1;
const ELF_VERSION_CURRENT: u8 = 1;
const PT_LOAD: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_DYNSYM: u32 = 11;
const SHT_STRTAB: u32 = 3;

/// Errors encountered while validating an EFI ELF image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ElfError {
    #[error("ELF image is too small")]
    TooSmall,
    #[error("invalid ELF magic")]
    BadMagic,
    #[error("only ELF64 is supported")]
    UnsupportedClass,
    #[error("only little-endian ELF images are supported")]
    UnsupportedEndian,
    #[error("unsupported ELF version")]
    UnsupportedVersion,
    #[error("ELF machine does not match the target")]
    UnsupportedMachine,
    #[error("requested EFI entry symbol does not match the target")]
    UnsupportedEntrySymbol,
    #[error("ELF header offset is outside the image")]
    HeaderRange,
    #[error("ELF program header table is outside the image")]
    ProgramHeaderRange,
    #[error("ELF image has no loadable segments")]
    LoadSegmentMissing,
    #[error("invalid or overlapping ELF load segment")]
    SegmentRange,
    #[error("ELF load segment address overflows")]
    SegmentAddressOverflow,
    #[error("ELF symbol table is outside the image or malformed")]
    SymbolTableRange,
    #[error("required EFI entry symbol is missing")]
    EntrySymbolMissing,
    #[error("EFI entry symbol is not in an executable load segment")]
    EntryNotExecutable,
}

/// A validated ELF64 PT_LOAD segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadSegment {
    pub file_offset: u64,
    pub virtual_address: u64,
    pub physical_address: u64,
    pub file_size: u64,
    pub memory_size: u64,
    pub flags: u32,
}

/// A validated EFI ELF image, expressed at its link-time physical addresses.
///
/// Allocation, copying, cache maintenance, and target-specific load bias remain
/// owned by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfImage {
    segments: Vec<LoadSegment>,
    entry_point: u64,
    link_load_address: u64,
    load_end: u64,
    page_count: usize,
}

impl ElfImage {
    /// Validates an ELF64 image against a target contract and resolves its EFI
    /// entry symbol to an executable PT_LOAD mapping.
    ///
    /// `requested_entry_symbol` is the protocol-visible symbol. It must match
    /// the symbol named by `spec` before the image's symbol table is searched.
    pub fn parse(
        image: &[u8],
        spec: TargetSpec,
        requested_entry_symbol: Option<&str>,
    ) -> Result<Self, ElfError> {
        let header = Elf64Header::read(image).ok_or(ElfError::TooSmall)?;
        validate_header(&header, spec, requested_entry_symbol)?;
        let segments = load_segments(image, &header)?;

        let first_physical_address = segments
            .iter()
            .map(|segment| segment.physical_address)
            .min()
            .ok_or(ElfError::LoadSegmentMissing)?;
        let last_physical_end = segments
            .iter()
            .try_fold(0_u64, |last, segment| {
                segment
                    .physical_address
                    .checked_add(segment.memory_size)
                    .map(|end| last.max(end))
            })
            .ok_or(ElfError::SegmentAddressOverflow)?;
        let link_load_address = align_down(first_physical_address);
        let load_end = align_up(last_physical_end).ok_or(ElfError::SegmentAddressOverflow)?;
        if load_end < link_load_address {
            return Err(ElfError::SegmentAddressOverflow);
        }
        let span = load_end - link_load_address;
        let page_count =
            usize::try_from(span / EFI_PAGE_SIZE).map_err(|_| ElfError::SegmentAddressOverflow)?;

        let entry_symbol =
            find_symbol(image, &header, spec.entry_symbol)?.ok_or(ElfError::EntrySymbolMissing)?;
        let entry_point = virtual_to_executable_physical(entry_symbol, &segments)
            .ok_or(ElfError::EntryNotExecutable)?;

        Ok(Self {
            segments,
            entry_point,
            link_load_address,
            load_end,
            page_count,
        })
    }

    /// Returns validated PT_LOAD segments sorted by physical address.
    pub fn segments(&self) -> &[LoadSegment] {
        &self.segments
    }

    /// Returns the entry point in the image's link-time physical address space.
    pub fn entry_point(&self) -> u64 {
        self.entry_point
    }

    /// Returns the page-aligned first link-time physical load address.
    pub fn link_load_address(&self) -> u64 {
        self.link_load_address
    }

    /// Returns the page-aligned exclusive end of the link-time load span.
    pub fn load_end(&self) -> u64 {
        self.load_end
    }

    /// Returns the number of four-KiB pages occupied by the image.
    pub fn page_count(&self) -> usize {
        self.page_count
    }
}

#[derive(Debug, Clone, Copy)]
struct Elf64Header {
    ident: [u8; 7],
    class: u8,
    data: u8,
    ident_version: u8,
    machine: u16,
    version: u32,
    program_header_offset: u64,
    section_header_offset: u64,
    program_header_size: u16,
    program_header_count: u16,
    section_header_size: u16,
    section_header_count: u16,
}

impl Elf64Header {
    fn read(image: &[u8]) -> Option<Self> {
        if image.len() < ELF64_HEADER_SIZE {
            return None;
        }
        let mut ident = [0; 7];
        ident.copy_from_slice(image.get(..7)?);
        Some(Self {
            ident,
            class: *image.get(4)?,
            data: *image.get(5)?,
            ident_version: *image.get(6)?,
            machine: read_u16(image, 18)?,
            version: read_u32(image, 20)?,
            program_header_offset: read_u64(image, 32)?,
            section_header_offset: read_u64(image, 40)?,
            program_header_size: read_u16(image, 54)?,
            program_header_count: read_u16(image, 56)?,
            section_header_size: read_u16(image, 58)?,
            section_header_count: read_u16(image, 60)?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct Elf64ProgramHeader {
    segment_type: u32,
    flags: u32,
    file_offset: u64,
    virtual_address: u64,
    physical_address: u64,
    file_size: u64,
    memory_size: u64,
}

impl Elf64ProgramHeader {
    fn read(image: &[u8], offset: usize) -> Option<Self> {
        Some(Self {
            segment_type: read_u32(image, offset)?,
            flags: read_u32(image, offset.checked_add(4)?)?,
            file_offset: read_u64(image, offset.checked_add(8)?)?,
            virtual_address: read_u64(image, offset.checked_add(16)?)?,
            physical_address: read_u64(image, offset.checked_add(24)?)?,
            file_size: read_u64(image, offset.checked_add(32)?)?,
            memory_size: read_u64(image, offset.checked_add(40)?)?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct Elf64SectionHeader {
    section_type: u32,
    data_offset: u64,
    data_size: u64,
    link: u32,
    entry_size: u64,
}

impl Elf64SectionHeader {
    fn read(image: &[u8], offset: usize) -> Option<Self> {
        Some(Self {
            section_type: read_u32(image, offset.checked_add(4)?)?,
            data_offset: read_u64(image, offset.checked_add(24)?)?,
            data_size: read_u64(image, offset.checked_add(32)?)?,
            link: read_u32(image, offset.checked_add(40)?)?,
            entry_size: read_u64(image, offset.checked_add(56)?)?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct Elf64Symbol {
    name_offset: u32,
    value: u64,
}

impl Elf64Symbol {
    fn read(image: &[u8], offset: usize) -> Option<Self> {
        Some(Self {
            name_offset: read_u32(image, offset)?,
            value: read_u64(image, offset.checked_add(8)?)?,
        })
    }
}

fn validate_header(
    header: &Elf64Header,
    spec: TargetSpec,
    requested_entry_symbol: Option<&str>,
) -> Result<(), ElfError> {
    if header.ident[..4] != *ELF_MAGIC {
        return Err(ElfError::BadMagic);
    }
    if header.class != ELF_CLASS_64 {
        return Err(ElfError::UnsupportedClass);
    }
    if header.data != ELF_DATA_LSB {
        return Err(ElfError::UnsupportedEndian);
    }
    if header.ident_version != ELF_VERSION_CURRENT
        || header.version != u32::from(ELF_VERSION_CURRENT)
    {
        return Err(ElfError::UnsupportedVersion);
    }
    if !spec.matches_machine(header.machine) {
        return Err(ElfError::UnsupportedMachine);
    }
    if requested_entry_symbol != Some(spec.entry_symbol) {
        return Err(ElfError::UnsupportedEntrySymbol);
    }
    Ok(())
}

fn load_segments(image: &[u8], header: &Elf64Header) -> Result<Vec<LoadSegment>, ElfError> {
    if usize::from(header.program_header_size) != ELF64_PROGRAM_HEADER_SIZE {
        return Err(ElfError::ProgramHeaderRange);
    }
    let table_offset =
        usize::try_from(header.program_header_offset).map_err(|_| ElfError::HeaderRange)?;
    let entry_size = usize::from(header.program_header_size);
    let table_size = usize::from(header.program_header_count)
        .checked_mul(entry_size)
        .ok_or(ElfError::ProgramHeaderRange)?;
    let table_end = table_offset
        .checked_add(table_size)
        .ok_or(ElfError::ProgramHeaderRange)?;
    if table_end > image.len() {
        return Err(ElfError::ProgramHeaderRange);
    }

    let mut segments = Vec::new();
    for index in 0..usize::from(header.program_header_count) {
        let offset = table_offset + index * entry_size;
        let header = Elf64ProgramHeader::read(image, offset).ok_or(ElfError::ProgramHeaderRange)?;
        if header.segment_type != PT_LOAD {
            continue;
        }
        if header.memory_size == 0 {
            return Err(ElfError::SegmentRange);
        }
        if header.file_size > header.memory_size {
            return Err(ElfError::SegmentRange);
        }
        let file_start = usize::try_from(header.file_offset).map_err(|_| ElfError::SegmentRange)?;
        let file_size = usize::try_from(header.file_size).map_err(|_| ElfError::SegmentRange)?;
        let file_end = file_start
            .checked_add(file_size)
            .ok_or(ElfError::SegmentRange)?;
        if file_end > image.len() {
            return Err(ElfError::SegmentRange);
        }
        let virtual_end = header
            .virtual_address
            .checked_add(header.memory_size)
            .ok_or(ElfError::SegmentAddressOverflow)?;
        if virtual_end <= header.virtual_address {
            return Err(ElfError::SegmentAddressOverflow);
        }
        segments.push(LoadSegment {
            file_offset: header.file_offset,
            virtual_address: header.virtual_address,
            physical_address: header.physical_address,
            file_size: header.file_size,
            memory_size: header.memory_size,
            flags: header.flags,
        });
    }

    if segments.is_empty() {
        return Err(ElfError::LoadSegmentMissing);
    }
    segments.sort_by_key(|segment| segment.physical_address);
    if segments.windows(2).any(|pair| {
        ranges_overlap(
            pair[0].physical_address,
            pair[0].memory_size,
            pair[1].physical_address,
        )
    }) {
        return Err(ElfError::SegmentRange);
    }
    Ok(segments)
}

fn find_symbol(image: &[u8], header: &Elf64Header, name: &str) -> Result<Option<u64>, ElfError> {
    let section_count = usize::from(header.section_header_count);
    if section_count == 0 {
        return Ok(None);
    }
    let section_size = usize::from(header.section_header_size);
    if section_size < ELF64_SECTION_HEADER_SIZE {
        return Err(ElfError::SymbolTableRange);
    }
    let table_offset =
        usize::try_from(header.section_header_offset).map_err(|_| ElfError::SymbolTableRange)?;
    let table_size = section_count
        .checked_mul(section_size)
        .ok_or(ElfError::SymbolTableRange)?;
    let table_end = table_offset
        .checked_add(table_size)
        .ok_or(ElfError::SymbolTableRange)?;
    if table_end > image.len() {
        return Err(ElfError::SymbolTableRange);
    }

    for index in 0..section_count {
        let section = Elf64SectionHeader::read(image, table_offset + index * section_size)
            .ok_or(ElfError::SymbolTableRange)?;
        if section.section_type != SHT_SYMTAB && section.section_type != SHT_DYNSYM {
            continue;
        }
        let string_index = usize::try_from(section.link).map_err(|_| ElfError::SymbolTableRange)?;
        if string_index >= section_count {
            return Err(ElfError::SymbolTableRange);
        }
        let string_section =
            Elf64SectionHeader::read(image, table_offset + string_index * section_size)
                .ok_or(ElfError::SymbolTableRange)?;
        if string_section.section_type != SHT_STRTAB {
            return Err(ElfError::SymbolTableRange);
        }
        let symbols = section_bytes(image, section.data_offset, section.data_size)?;
        let strings = section_bytes(image, string_section.data_offset, string_section.data_size)?;
        let entry_size = if section.entry_size == 0 {
            ELF64_SYMBOL_SIZE
        } else {
            section.entry_size
        };
        if entry_size < ELF64_SYMBOL_SIZE {
            return Err(ElfError::SymbolTableRange);
        }
        let entry_size = usize::try_from(entry_size).map_err(|_| ElfError::SymbolTableRange)?;
        if symbols.len() % entry_size != 0 {
            return Err(ElfError::SymbolTableRange);
        }
        let mut offset = 0usize;
        while offset < symbols.len() {
            let symbol = Elf64Symbol::read(symbols, offset).ok_or(ElfError::SymbolTableRange)?;
            let name_offset =
                usize::try_from(symbol.name_offset).map_err(|_| ElfError::SymbolTableRange)?;
            if cstr_at(strings, name_offset).is_some_and(|symbol_name| symbol_name == name) {
                return Ok(Some(symbol.value));
            }
            offset += entry_size;
        }
    }
    Ok(None)
}

fn section_bytes(image: &[u8], offset: u64, size: u64) -> Result<&[u8], ElfError> {
    let start = usize::try_from(offset).map_err(|_| ElfError::SymbolTableRange)?;
    let len = usize::try_from(size).map_err(|_| ElfError::SymbolTableRange)?;
    let end = start.checked_add(len).ok_or(ElfError::SymbolTableRange)?;
    image.get(start..end).ok_or(ElfError::SymbolTableRange)
}

fn cstr_at(bytes: &[u8], offset: usize) -> Option<&str> {
    let tail = bytes.get(offset..)?;
    let len = tail.iter().position(|byte| *byte == 0)?;
    core::str::from_utf8(&tail[..len]).ok()
}

fn virtual_to_executable_physical(value: u64, segments: &[LoadSegment]) -> Option<u64> {
    segments.iter().find_map(|segment| {
        let offset = value.checked_sub(segment.virtual_address)?;
        (segment.flags & ELF_PF_X != 0 && offset < segment.memory_size)
            .then(|| segment.physical_address.checked_add(offset))
            .flatten()
    })
}

fn align_up(value: u64) -> Option<u64> {
    value
        .checked_add(EFI_PAGE_SIZE - 1)
        .map(|value| value & !(EFI_PAGE_SIZE - 1))
}

fn align_down(value: u64) -> u64 {
    value & !(EFI_PAGE_SIZE - 1)
}

fn read_u16(image: &[u8], offset: usize) -> Option<u16> {
    let bytes = image.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(image: &[u8], offset: usize) -> Option<u32> {
    let bytes = image.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(image: &[u8], offset: usize) -> Option<u64> {
    let bytes = image.get(offset..offset.checked_add(8)?)?;
    Some(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::{ELF_PF_X, ElfError, ElfImage};
    use crate::target::{LOONGARCH64_EFI, TargetSpec, X86_64};

    struct TestSegment {
        flags: u32,
        virtual_address: u64,
        physical_address: u64,
        memory_size: u64,
    }

    fn write_u16(image: &mut [u8], offset: usize, value: u16) {
        image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(image: &mut [u8], offset: usize, value: u64) {
        image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn align_mut(image: &mut Vec<u8>, alignment: usize) -> usize {
        while image.len() % alignment != 0 {
            image.push(0);
        }
        image.len()
    }

    fn test_elf(spec: TargetSpec, segments: &[TestSegment]) -> Vec<u8> {
        let mut image = vec![0; 64 + segments.len() * 56];
        image[..4].copy_from_slice(b"\x7fELF");
        image[4] = 2;
        image[5] = 1;
        image[6] = 1;
        write_u16(&mut image, 16, 2);
        write_u16(&mut image, 18, spec.machine);
        write_u32(&mut image, 20, 1);
        write_u64(&mut image, 32, 64);
        write_u16(&mut image, 52, 64);
        write_u16(&mut image, 54, 56);
        write_u16(&mut image, 56, segments.len() as u16);
        write_u16(&mut image, 58, 64);
        write_u16(&mut image, 60, 3);

        let first_offset = image.len() as u64;
        for (index, segment) in segments.iter().enumerate() {
            let offset = 64 + index * 56;
            write_u32(&mut image, offset, 1);
            write_u32(&mut image, offset + 4, segment.flags);
            write_u64(&mut image, offset + 8, first_offset + (index as u64) * 16);
            write_u64(&mut image, offset + 16, segment.virtual_address);
            write_u64(&mut image, offset + 24, segment.physical_address);
            write_u64(&mut image, offset + 32, 16);
            write_u64(&mut image, offset + 40, segment.memory_size);
            write_u64(&mut image, offset + 48, 1);
        }
        for _ in 0..segments.len() {
            image.extend_from_slice(&[0x90; 16]);
        }

        let symbol_offset = align_mut(&mut image, 8);
        image.extend_from_slice(&[0; 24]);
        let mut entry_symbol = [0; 24];
        entry_symbol[0..4].copy_from_slice(&1u32.to_le_bytes());
        entry_symbol[8..16].copy_from_slice(&segments[0].virtual_address.to_le_bytes());
        image.extend_from_slice(&entry_symbol);
        let string_offset = image.len();
        let mut strings = alloc::vec![0];
        strings.extend_from_slice(spec.entry_symbol.as_bytes());
        strings.push(0);
        let string_size = strings.len() as u64;
        image.extend_from_slice(&strings);

        let section_offset = align_mut(&mut image, 8);
        image.resize(section_offset + 3 * 64, 0);
        write_u64(&mut image, 40, section_offset as u64);
        let symbol_section = section_offset + 64;
        write_u32(&mut image, symbol_section + 4, 2);
        write_u64(&mut image, symbol_section + 24, symbol_offset as u64);
        write_u64(&mut image, symbol_section + 32, 48);
        write_u32(&mut image, symbol_section + 40, 2);
        write_u32(&mut image, symbol_section + 44, 1);
        write_u64(&mut image, symbol_section + 56, 24);
        let string_section = section_offset + 128;
        write_u32(&mut image, string_section + 4, 3);
        write_u64(&mut image, string_section + 24, string_offset as u64);
        write_u64(&mut image, string_section + 32, string_size);
        image
    }

    fn parse(spec: TargetSpec, segments: &[TestSegment]) -> Result<ElfImage, ElfError> {
        let image = test_elf(spec, segments);
        ElfImage::parse(&image, spec, Some(spec.entry_symbol))
    }

    fn executable_segment(spec: TargetSpec) -> TestSegment {
        let address = if spec.load_address.is_some() {
            spec.link_address.unwrap()
        } else {
            0x10_0123
        };
        TestSegment {
            flags: ELF_PF_X | 4,
            virtual_address: address,
            physical_address: address,
            memory_size: 0x2000,
        }
    }

    #[test]
    fn valid_x86_and_loong_images_parse_link_time_layout() {
        for spec in [X86_64, LOONGARCH64_EFI] {
            let image = parse(spec, &[executable_segment(spec)]).unwrap();
            let expected_link = if spec.load_address.is_some() {
                spec.link_address.unwrap()
            } else {
                0x10_0000
            };
            let expected_entry = if spec.load_address.is_some() {
                spec.link_address.unwrap()
            } else {
                0x10_0123
            };
            let (expected_end, expected_pages) = if spec.load_address.is_some() {
                (expected_link + 0x2000, 2)
            } else {
                (expected_link + 0x3000, 3)
            };
            assert_eq!(image.entry_point(), expected_entry);
            assert_eq!(image.link_load_address(), expected_link);
            assert_eq!(image.load_end(), expected_end);
            assert_eq!(image.page_count(), expected_pages);
            assert_eq!(image.segments().len(), 1);
            assert!(image.segments()[0].flags & ELF_PF_X != 0);
        }
    }

    #[test]
    fn wrong_machine_or_entry_symbol_is_rejected() {
        let mut image = test_elf(X86_64, &[executable_segment(X86_64)]);
        write_u16(&mut image, 18, LOONGARCH64_EFI.machine);
        assert_eq!(
            ElfImage::parse(&image, X86_64, Some(X86_64.entry_symbol)).unwrap_err(),
            ElfError::UnsupportedMachine
        );

        let image = test_elf(X86_64, &[executable_segment(X86_64)]);
        assert_eq!(
            ElfImage::parse(&image, X86_64, Some("httpboot_entry")).unwrap_err(),
            ElfError::UnsupportedEntrySymbol
        );
    }

    #[test]
    fn entry_symbol_must_map_to_an_executable_load_segment() {
        let mut segment = executable_segment(X86_64);
        segment.flags = 4;
        assert_eq!(
            parse(X86_64, &[segment]).unwrap_err(),
            ElfError::EntryNotExecutable
        );

        let mut image = test_elf(X86_64, &[executable_segment(X86_64)]);
        let section_offset = read_test_u64(&image, 40) as usize;
        let symbol_section = section_offset + 64;
        write_u32(&mut image, symbol_section + 4, 1);
        assert_eq!(
            ElfImage::parse(&image, X86_64, Some(X86_64.entry_symbol)).unwrap_err(),
            ElfError::EntrySymbolMissing
        );
    }

    #[test]
    fn program_headers_and_segment_file_ranges_are_rejected() {
        let mut image = test_elf(X86_64, &[executable_segment(X86_64)]);
        image.truncate(63);
        assert_eq!(
            ElfImage::parse(&image, X86_64, Some(X86_64.entry_symbol)).unwrap_err(),
            ElfError::TooSmall
        );

        let mut image = test_elf(X86_64, &[executable_segment(X86_64)]);
        let length = image.len() as u64;
        write_u64(&mut image, 32, length);
        assert_eq!(
            ElfImage::parse(&image, X86_64, Some(X86_64.entry_symbol)).unwrap_err(),
            ElfError::ProgramHeaderRange
        );

        let mut image = test_elf(X86_64, &[executable_segment(X86_64)]);
        let length = image.len() as u64;
        write_u64(&mut image, 72, length);
        write_u64(&mut image, 80, length);
        assert_eq!(
            ElfImage::parse(&image, X86_64, Some(X86_64.entry_symbol)).unwrap_err(),
            ElfError::SegmentRange
        );
    }

    #[test]
    fn zero_overflowing_and_overlapping_segments_are_rejected() {
        let mut segment = executable_segment(X86_64);
        segment.memory_size = 0;
        assert_eq!(
            parse(X86_64, &[segment]).unwrap_err(),
            ElfError::SegmentRange
        );

        let mut segment = executable_segment(X86_64);
        segment.physical_address = u64::MAX - 4095;
        segment.virtual_address = u64::MAX - 4095;
        assert_eq!(
            parse(X86_64, &[segment]).unwrap_err(),
            ElfError::SegmentAddressOverflow
        );

        let first = executable_segment(X86_64);
        let mut second = executable_segment(X86_64);
        second.virtual_address += 0x1000;
        second.physical_address += 0x1000;
        assert_eq!(
            parse(X86_64, &[first, second]).unwrap_err(),
            ElfError::SegmentRange
        );
    }

    fn read_test_u64(image: &[u8], offset: usize) -> u64 {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(&image[offset..offset + 8]);
        u64::from_le_bytes(bytes)
    }
}

const ELF64_HEADER_SIZE: usize = 64;
const ELF64_PROGRAM_HEADER_SIZE: usize = 56;
const ELF64_SECTION_HEADER_SIZE: usize = 64;
const ELF64_SYMBOL_SIZE: u64 = 24;
