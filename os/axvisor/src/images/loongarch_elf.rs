use ax_errno::{AxResult, ax_err_type};
use axvm::{AxVMRef, GuestPhysAddr};

use crate::images::{load_vm_image_from_memory, zero_vm_memory};

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LE: u8 = 1;
const EM_LOONGARCH: u16 = 258;
const PT_LOAD: u32 = 1;
const LOONGARCH_DMW_MASK: u64 = (1u64 << 48) - 1;

#[derive(Clone, Copy, Debug)]
pub struct ElfInfo {
    pub entry: GuestPhysAddr,
}

#[derive(Clone, Copy, Debug)]
struct ProgramHeader {
    p_type: u32,
    p_offset: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
}

pub fn try_load(image: &[u8], vm: AxVMRef) -> AxResult<Option<ElfInfo>> {
    let Some(header) = Header::parse(image)? else {
        return Ok(None);
    };

    for idx in 0..header.e_phnum {
        let ph = read_program_header(image, &header, idx)?;
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }

        let file_start = ph.p_offset as usize;
        let file_size = ph.p_filesz as usize;
        let file_end = file_start.checked_add(file_size).ok_or_else(|| {
            ax_err_type!(
                InvalidInput,
                "LoongArch ELF LOAD segment file range overflows usize"
            )
        })?;
        let mem_size = ph.p_memsz as usize;
        if file_end > image.len() {
            return Err(ax_err_type!(
                InvalidInput,
                format!(
                    "LoongArch ELF LOAD segment exceeds image size: offset {:#x}, filesz {:#x}, image {:#x}",
                    ph.p_offset,
                    ph.p_filesz,
                    image.len()
                )
            ));
        }
        if ph.p_filesz > ph.p_memsz {
            return Err(ax_err_type!(
                InvalidInput,
                "LoongArch ELF LOAD segment filesz is larger than memsz"
            ));
        }

        let load_gpa = GuestPhysAddr::from((ph.p_paddr & LOONGARCH_DMW_MASK) as usize);
        info!(
            "Loading LoongArch ELF segment: paddr={:#x} -> gpa={:#x}, filesz={:#x}, memsz={:#x}",
            ph.p_paddr,
            load_gpa.as_usize(),
            ph.p_filesz,
            ph.p_memsz
        );

        zero_vm_memory(load_gpa, mem_size, vm.clone())?;
        load_vm_image_from_memory(&image[file_start..file_end], load_gpa, vm.clone())?;
    }

    Ok(Some(ElfInfo {
        entry: GuestPhysAddr::from(header.e_entry as usize),
    }))
}

struct Header {
    e_entry: u64,
    e_phoff: u64,
    e_phentsize: u16,
    e_phnum: u16,
}

impl Header {
    fn parse(image: &[u8]) -> AxResult<Option<Self>> {
        if image.len() < 64 || &image[0..4] != ELF_MAGIC {
            return Ok(None);
        }
        if image[4] != ELF_CLASS_64 || image[5] != ELF_DATA_LE {
            return Err(ax_err_type!(
                InvalidInput,
                "LoongArch kernel ELF must be 64-bit little-endian"
            ));
        }
        let machine = read_u16(image, 18)?;
        if machine != EM_LOONGARCH {
            return Ok(None);
        }

        let e_phentsize = read_u16(image, 54)?;
        if e_phentsize < 56 {
            return Err(ax_err_type!(
                InvalidInput,
                format!("LoongArch ELF program header is too small: {e_phentsize}")
            ));
        }

        Ok(Some(Self {
            e_entry: read_u64(image, 24)?,
            e_phoff: read_u64(image, 32)?,
            e_phentsize,
            e_phnum: read_u16(image, 56)?,
        }))
    }
}

fn read_program_header(image: &[u8], header: &Header, idx: u16) -> AxResult<ProgramHeader> {
    let start = (header.e_phoff as usize)
        .checked_add(idx as usize * header.e_phentsize as usize)
        .ok_or_else(|| ax_err_type!(InvalidInput, "LoongArch ELF phdr offset overflow"))?;
    let end = start
        .checked_add(header.e_phentsize as usize)
        .ok_or_else(|| ax_err_type!(InvalidInput, "LoongArch ELF phdr end overflow"))?;
    if end > image.len() {
        return Err(ax_err_type!(
            InvalidInput,
            "LoongArch ELF program header exceeds image size"
        ));
    }

    Ok(ProgramHeader {
        p_type: read_u32(image, start)?,
        p_offset: read_u64(image, start + 8)?,
        p_paddr: read_u64(image, start + 24)?,
        p_filesz: read_u64(image, start + 32)?,
        p_memsz: read_u64(image, start + 40)?,
    })
}

fn read_u16(image: &[u8], offset: usize) -> AxResult<u16> {
    let bytes = image
        .get(offset..offset + 2)
        .ok_or_else(|| ax_err_type!(InvalidInput, "LoongArch ELF u16 read out of range"))?;
    Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u32(image: &[u8], offset: usize) -> AxResult<u32> {
    let bytes = image
        .get(offset..offset + 4)
        .ok_or_else(|| ax_err_type!(InvalidInput, "LoongArch ELF u32 read out of range"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u64(image: &[u8], offset: usize) -> AxResult<u64> {
    let bytes = image
        .get(offset..offset + 8)
        .ok_or_else(|| ax_err_type!(InvalidInput, "LoongArch ELF u64 read out of range"))?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}
