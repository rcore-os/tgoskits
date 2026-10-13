//! Shared LoongArch EFI PE layout validation.

extern crate alloc;

use alloc::vec::Vec;

use crate::http_control::MAX_IMAGE_BYTES;
const PE_MACHINE_LOONGARCH64: u16 = 0x6264;
const PE32_PLUS_MAGIC: u16 = 0x20b;
const PE32_PLUS_OPTIONAL_HEADER_SIZE: usize = 112;
const PE_DATA_DIRECTORY_OFFSET: usize = 112;
const PE_BASE_RELOCATION_DIRECTORY: usize = 5;
const PE_SECTION_HEADER_SIZE: usize = 40;
const PE_SECTION_EXECUTE: u32 = 0x2000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    pub virtual_address: usize,
    pub virtual_size: usize,
    pub raw_offset: usize,
    pub raw_size: usize,
    pub characteristics: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Layout {
    pub headers: usize,
    pub sections: Vec<Section>,
    pub image_size: usize,
    pub entry: usize,
}

impl Layout {
    pub fn entry_is_executable(&self) -> bool {
        self.sections.iter().any(|section| {
            section.characteristics & PE_SECTION_EXECUTE != 0
                && self.entry >= section.virtual_address
                && self.entry
                    < section
                        .virtual_address
                        .saturating_add(section.virtual_size.max(section.raw_size))
        })
    }

    pub fn validate(&self, file_size: usize) -> Result<(), &'static str> {
        if self.headers > self.image_size
            || self.entry < self.headers
            || self.entry >= self.image_size
        {
            return Err("invalid EFI image layout");
        }
        let mut ranges = Vec::with_capacity(self.sections.len());
        for section in &self.sections {
            let virtual_end = section
                .virtual_address
                .checked_add(section.virtual_size.max(section.raw_size))
                .ok_or("EFI section range overflows")?;
            if virtual_end > self.image_size {
                return Err("EFI section is outside the image");
            }
            let raw_end = section
                .raw_offset
                .checked_add(section.raw_size)
                .ok_or("EFI section file range overflows")?;
            if raw_end > file_size {
                return Err("EFI section is outside the file");
            }
            if section.virtual_address < self.headers {
                return Err("EFI section overlaps PE headers");
            }
            ranges.push((section.virtual_address, virtual_end));
        }
        if !self.entry_is_executable() {
            return Err("EFI entry is outside executable sections");
        }
        ranges.sort_unstable_by_key(|(start, _)| *start);
        if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err("EFI sections overlap");
        }
        Ok(())
    }
}

pub fn layout(image: &[u8]) -> Result<Layout, &'static str> {
    if image.len() < 0x40 || &image[..2] != b"MZ" {
        return Err("invalid EFI image signature");
    }
    let pe_offset = read_u32(image, 0x3c)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or("invalid EFI PE offset")?;
    if pe_offset
        .checked_add(24)
        .is_none_or(|end| end > image.len())
        || &image[pe_offset..pe_offset + 4] != b"PE\0\0"
    {
        return Err("invalid EFI PE signature");
    }
    let machine = read_u16(image, pe_offset + 4).ok_or("missing EFI PE machine")?;
    if machine != PE_MACHINE_LOONGARCH64 {
        return Err("unsupported EFI PE machine");
    }
    let section_count =
        usize::from(read_u16(image, pe_offset + 6).ok_or("missing EFI PE section count")?);
    let optional_size =
        usize::from(read_u16(image, pe_offset + 20).ok_or("missing EFI optional header size")?);
    let optional = pe_offset.checked_add(24).ok_or("EFI PE offset overflows")?;
    if optional
        .checked_add(optional_size)
        .is_none_or(|end| end > image.len())
        || read_u16(image, optional) != Some(PE32_PLUS_MAGIC)
    {
        return Err("unsupported EFI PE optional header");
    }
    if optional_size < PE32_PLUS_OPTIONAL_HEADER_SIZE {
        return Err("truncated EFI PE optional header");
    }
    if read_u64(image, optional + 24) != Some(0) {
        return Err("unsupported EFI image base");
    }
    let directory_count =
        read_u32(image, optional + 108).ok_or("missing EFI PE data-directory count")?;
    if directory_count > PE_BASE_RELOCATION_DIRECTORY as u32 {
        let relocation_directory = optional
            .checked_add(PE_DATA_DIRECTORY_OFFSET)
            .and_then(|offset| offset.checked_add(PE_BASE_RELOCATION_DIRECTORY.checked_mul(8)?))
            .ok_or("EFI PE relocation directory overflows")?;
        if relocation_directory
            .checked_add(8)
            .is_none_or(|end| end > optional + optional_size)
        {
            return Err("truncated EFI PE relocation directory");
        }
        if read_u32(image, relocation_directory) != Some(0)
            || read_u32(image, relocation_directory + 4) != Some(0)
        {
            return Err("EFI PE base relocations are unsupported");
        }
    }
    let entry = usize::try_from(read_u32(image, optional + 16).ok_or("missing EFI PE entry")?)
        .map_err(|_| "EFI PE entry overflows")?;
    let image_size = usize::try_from(read_u32(image, optional + 56).ok_or("missing EFI PE size")?)
        .map_err(|_| "EFI PE size overflows")?;
    let headers = usize::try_from(read_u32(image, optional + 60).ok_or("missing EFI PE headers")?)
        .map_err(|_| "EFI PE headers overflow")?;
    if image_size == 0 || image_size > MAX_IMAGE_BYTES || headers > image.len() {
        return Err("invalid EFI PE size");
    }
    let section_table = optional
        .checked_add(optional_size)
        .ok_or("EFI section table overflows")?;
    let section_bytes = section_count
        .checked_mul(PE_SECTION_HEADER_SIZE)
        .ok_or("EFI section table size overflows")?;
    if section_table
        .checked_add(section_bytes)
        .is_none_or(|end| end > image.len())
    {
        return Err("EFI section table is truncated");
    }
    let mut sections = Vec::with_capacity(section_count);
    for index in 0..section_count {
        let offset = section_table + index * PE_SECTION_HEADER_SIZE;
        let virtual_size =
            usize::try_from(read_u32(image, offset + 8).ok_or("missing EFI section size")?)
                .map_err(|_| "EFI section size overflows")?;
        let virtual_address =
            usize::try_from(read_u32(image, offset + 12).ok_or("missing EFI section address")?)
                .map_err(|_| "EFI section address overflows")?;
        let raw_size =
            usize::try_from(read_u32(image, offset + 16).ok_or("missing EFI section raw size")?)
                .map_err(|_| "EFI section raw size overflows")?;
        let raw_offset =
            usize::try_from(read_u32(image, offset + 20).ok_or("missing EFI section raw offset")?)
                .map_err(|_| "EFI section raw offset overflows")?;
        let characteristics =
            read_u32(image, offset + 36).ok_or("missing EFI section characteristics")?;
        sections.push(Section {
            virtual_address,
            virtual_size,
            raw_offset,
            raw_size,
            characteristics,
        });
    }
    Ok(Layout {
        headers,
        sections,
        image_size,
        entry,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    bytes
        .get(offset..offset.checked_add(2)?)
        .map(|value| u16::from_le_bytes(value.try_into().unwrap()))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset.checked_add(4)?)
        .map(|value| u32::from_le_bytes(value.try_into().unwrap()))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    bytes
        .get(offset..offset.checked_add(8)?)
        .map(|value| u64::from_le_bytes(value.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(machine: u16, entry: u32, second_address: u32) -> Vec<u8> {
        let pe_offset = 0x40;
        let optional = pe_offset + 24;
        let section_table = optional + PE32_PLUS_OPTIONAL_HEADER_SIZE;
        let mut bytes = vec![0; section_table + PE_SECTION_HEADER_SIZE * 2 + 0x400];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&(pe_offset as u32).to_le_bytes());
        bytes[pe_offset..pe_offset + 4].copy_from_slice(b"PE\0\0");
        bytes[pe_offset + 4..pe_offset + 6].copy_from_slice(&machine.to_le_bytes());
        bytes[pe_offset + 6..pe_offset + 8].copy_from_slice(&2_u16.to_le_bytes());
        bytes[pe_offset + 20..pe_offset + 22]
            .copy_from_slice(&(PE32_PLUS_OPTIONAL_HEADER_SIZE as u16).to_le_bytes());
        bytes[optional..optional + 2].copy_from_slice(&PE32_PLUS_MAGIC.to_le_bytes());
        bytes[optional + 16..optional + 20].copy_from_slice(&entry.to_le_bytes());
        bytes[optional + 56..optional + 60].copy_from_slice(&0x400_u32.to_le_bytes());
        bytes[optional + 60..optional + 64].copy_from_slice(&0x200_u32.to_le_bytes());
        bytes[optional + 108..optional + 112].copy_from_slice(&0_u32.to_le_bytes());
        let first = section_table;
        bytes[first + 8..first + 12].copy_from_slice(&0x100_u32.to_le_bytes());
        bytes[first + 12..first + 16].copy_from_slice(&0x200_u32.to_le_bytes());
        bytes[first + 16..first + 20].copy_from_slice(&0x100_u32.to_le_bytes());
        bytes[first + 20..first + 24].copy_from_slice(&0x300_u32.to_le_bytes());
        bytes[first + 36..first + 40].copy_from_slice(&PE_SECTION_EXECUTE.to_le_bytes());
        let second = first + PE_SECTION_HEADER_SIZE;
        bytes[second + 8..second + 12].copy_from_slice(&0x80_u32.to_le_bytes());
        bytes[second + 12..second + 16].copy_from_slice(&second_address.to_le_bytes());
        bytes[second + 16..second + 20].copy_from_slice(&0_u32.to_le_bytes());
        bytes
    }

    #[test]
    fn rejects_wrong_machine_and_non_executable_entry() {
        assert_eq!(
            layout(&image(0x8664, 0x200, 0x320)),
            Err("unsupported EFI PE machine")
        );
        let image = image(PE_MACHINE_LOONGARCH64, 0x100, 0x320);
        let layout = layout(&image).unwrap();
        assert_eq!(
            layout.validate(image.len()),
            Err("invalid EFI image layout")
        );
    }

    #[test]
    fn rejects_overlapping_sections_and_accepts_valid_layout() {
        let overlap = image(PE_MACHINE_LOONGARCH64, 0x220, 0x280);
        assert_eq!(
            layout(&overlap).unwrap().validate(overlap.len()),
            Err("EFI sections overlap")
        );
        let valid = image(PE_MACHINE_LOONGARCH64, 0x220, 0x320);
        let layout = layout(&valid).unwrap();
        assert!(layout.entry_is_executable());
        assert!(layout.validate(valid.len()).is_ok());
    }

    #[test]
    fn rejects_relocated_images() {
        let pe_offset = 0x40;
        let optional = pe_offset + 24;

        let mut nonzero_base = image(PE_MACHINE_LOONGARCH64, 0x220, 0x320);
        nonzero_base[optional + 24..optional + 32].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(layout(&nonzero_base), Err("unsupported EFI image base"));

        let mut relocations = image(PE_MACHINE_LOONGARCH64, 0x220, 0x320);
        relocations[pe_offset + 20..pe_offset + 22].copy_from_slice(&160_u16.to_le_bytes());
        relocations[optional + 108..optional + 112].copy_from_slice(&6_u32.to_le_bytes());
        let directory = optional + PE_DATA_DIRECTORY_OFFSET + PE_BASE_RELOCATION_DIRECTORY * 8;
        relocations[directory..directory + 4].copy_from_slice(&0x300_u32.to_le_bytes());
        assert_eq!(
            layout(&relocations),
            Err("EFI PE base relocations are unsupported")
        );
    }
}
