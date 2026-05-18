use dma_api::{DArray, DmaDirection};

use crate::osal::Kernel;

pub struct EventBuffer {
    pub buffer: DArray<u8>,
}

impl EventBuffer {
    pub fn new(size: usize, kernel: &Kernel) -> crate::err::Result<Self> {
        // let buffer = DVec::zeros(dma_mask as _, size, 0x1000, dma_api::Direction::FromDevice)
        //     .map_err(|_| crate::err::USBError::NoMemory)?;

        let buffer = kernel
            .array_zero_with_align(size, 0x1000, DmaDirection::FromDevice)
            .map_err(|_| crate::err::USBError::NoMemory)?;

        Ok(Self { buffer })
    }

    pub fn dma_addr(&self) -> u64 {
        self.buffer.dma_addr().as_u64()
    }
}
