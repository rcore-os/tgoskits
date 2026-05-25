use dma_api::{ContiguousArray, DmaDirection};

use crate::osal::Kernel;

pub struct EventBuffer {
    pub buffer: ContiguousArray<u8>,
}

impl EventBuffer {
    pub fn new(size: usize, kernel: &Kernel) -> crate::err::Result<Self> {
        let buffer = kernel
            .contiguous_array_zero_with_align(size, 0x1000, DmaDirection::FromDevice)
            .map_err(|_| crate::err::USBError::NoMemory)?;

        Ok(Self { buffer })
    }

    pub fn dma_addr(&self) -> u64 {
        self.buffer.dma_addr().as_u64()
    }
}
