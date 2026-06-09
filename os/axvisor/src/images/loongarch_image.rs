use ax_errno::{AxResult, ax_err_type};
use axvm::{AxVMRef, GuestPhysAddr};

use crate::images::load_vm_image_from_memory;

const LOONGARCH_IMAGE_MAGIC: &[u8; 8] = b"MZ\0\0\0\0\0\0";
const HEADER_SIZE: usize = 0x40;
const KERNEL_ENTRY_OFFSET: usize = 0x8;
const IMAGE_SIZE_OFFSET: usize = 0x10;
const LOAD_OFFSET_OFFSET: usize = 0x18;
const PE_POINTER_OFFSET: usize = 0x3c;
const PE_HEADER_OFFSET: u32 = 0x40;

#[derive(Clone, Copy, Debug)]
pub struct ImageInfo {
    pub entry: GuestPhysAddr,
}

pub fn try_load(image: &[u8], vm: AxVMRef) -> AxResult<Option<ImageInfo>> {
    let Some(header) = Header::parse(image)? else {
        return Ok(None);
    };

    let image = &image[..header.image_size];
    load_vm_image_from_memory(image, GuestPhysAddr::from(header.load_offset), vm)?;
    Ok(Some(ImageInfo {
        entry: GuestPhysAddr::from(header.entry),
    }))
}

struct Header {
    entry: usize,
    image_size: usize,
    load_offset: usize,
}

impl Header {
    fn parse(image: &[u8]) -> AxResult<Option<Self>> {
        if image.len() < HEADER_SIZE
            || &image[..LOONGARCH_IMAGE_MAGIC.len()] != LOONGARCH_IMAGE_MAGIC
        {
            return Ok(None);
        }
        if read_u32(image, PE_POINTER_OFFSET)? != PE_HEADER_OFFSET {
            return Ok(None);
        }

        let image_size = read_u64(image, IMAGE_SIZE_OFFSET)? as usize;
        if image_size == 0 || image.len() < image_size {
            return Err(ax_err_type!(
                InvalidInput,
                format!(
                    "LoongArch Linux Image size is invalid: image={:#x}, header={:#x}",
                    image.len(),
                    image_size
                )
            ));
        }

        Ok(Some(Self {
            entry: read_u64(image, KERNEL_ENTRY_OFFSET)? as usize,
            image_size,
            load_offset: read_u64(image, LOAD_OFFSET_OFFSET)? as usize,
        }))
    }
}

fn read_u32(image: &[u8], offset: usize) -> AxResult<u32> {
    let bytes = image
        .get(offset..offset + 4)
        .ok_or_else(|| ax_err_type!(InvalidInput, "LoongArch Image u32 read out of range"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u64(image: &[u8], offset: usize) -> AxResult<u64> {
    let bytes = image
        .get(offset..offset + 8)
        .ok_or_else(|| ax_err_type!(InvalidInput, "LoongArch Image u64 read out of range"))?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}
