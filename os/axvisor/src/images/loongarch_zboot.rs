use alloc::vec::Vec;

use ax_errno::{AxResult, ax_err_type};
use ruzstd::{decoding::StreamingDecoder, io::Read};

const ZBOOT_MAGIC: &[u8; 8] = b"MZ\0\0zimg";
const HEADER_SIZE: usize = 0x40;
const PAYLOAD_INFO_OFFSET: usize = 0x8;
const PAYLOAD_OFFSET_BITS: u64 = 32;
const PAYLOAD_OFFSET_MASK: u64 = (1 << PAYLOAD_OFFSET_BITS) - 1;
const FORMAT_OFFSET: usize = 0x18;
const FORMAT_ZSTD: &[u8; 4] = b"zstd";

#[derive(Debug)]
pub struct ZbootImage {
    pub payload: Vec<u8>,
}

pub fn decompress(image: &[u8]) -> AxResult<Option<ZbootImage>> {
    let Some(header) = Header::parse(image)? else {
        return Ok(None);
    };

    let compressed = image
        .get(header.payload_offset..header.payload_end())
        .ok_or_else(|| {
            ax_err_type!(
                InvalidInput,
                format!(
                    "LoongArch zboot payload range [{:#x}, {:#x}) exceeds image size {:#x}",
                    header.payload_offset,
                    header.payload_end(),
                    image.len()
                )
            )
        })?;

    let mut decoder = StreamingDecoder::new(compressed).map_err(|err| {
        ax_err_type!(
            InvalidInput,
            format!("failed to initialize LoongArch zboot zstd decoder: {err:?}")
        )
    })?;
    let mut payload = Vec::with_capacity(header.payload_size);
    decoder.read_to_end(&mut payload).map_err(|err| {
        ax_err_type!(
            InvalidInput,
            format!("failed to decompress LoongArch zboot zstd payload: {err}")
        )
    })?;

    info!(
        "Decompressed LoongArch zboot image: payload_offset={:#x}, compressed={:#x}, decompressed={:#x}",
        header.payload_offset,
        header.payload_size,
        payload.len()
    );

    Ok(Some(ZbootImage { payload }))
}

struct Header {
    payload_offset: usize,
    payload_size: usize,
}

impl Header {
    fn parse(image: &[u8]) -> AxResult<Option<Self>> {
        if image.len() < HEADER_SIZE || &image[..ZBOOT_MAGIC.len()] != ZBOOT_MAGIC {
            return Ok(None);
        }
        if image
            .get(FORMAT_OFFSET..FORMAT_OFFSET + FORMAT_ZSTD.len())
            .is_none_or(|format| format != FORMAT_ZSTD)
        {
            return Err(ax_err_type!(
                InvalidInput,
                "unsupported LoongArch zboot compression format"
            ));
        }

        let payload_info = read_u64(image, PAYLOAD_INFO_OFFSET)?;
        let payload_offset = (payload_info & PAYLOAD_OFFSET_MASK) as usize;
        let payload_size = (payload_info >> PAYLOAD_OFFSET_BITS) as usize;
        if payload_offset < HEADER_SIZE || payload_size == 0 {
            return Err(ax_err_type!(
                InvalidInput,
                format!(
                    "invalid LoongArch zboot payload metadata: offset={:#x}, size={:#x}",
                    payload_offset, payload_size
                )
            ));
        }

        Ok(Some(Self {
            payload_offset,
            payload_size,
        }))
    }

    fn payload_end(&self) -> usize {
        self.payload_offset.saturating_add(self.payload_size)
    }
}

fn read_u64(image: &[u8], offset: usize) -> AxResult<u64> {
    let bytes = image
        .get(offset..offset + 8)
        .ok_or_else(|| ax_err_type!(InvalidInput, "LoongArch zboot u64 read out of range"))?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}
