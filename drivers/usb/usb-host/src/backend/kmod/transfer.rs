use alloc::vec::Vec;
use core::ptr::NonNull;

use dma_api::DmaDirection;
use usb_if::{endpoint::TransferRequest, err::TransferError, transfer::Direction};

use crate::{
    backend::ty::transfer::{Transfer, TransferKind},
    osal::Kernel,
};

const ALIGN: usize = 64;

impl Transfer {
    pub(crate) fn new(
        dma: &Kernel,
        kind: TransferKind,
        direction: Direction,
        buff: Option<(NonNull<u8>, usize)>,
    ) -> Result<Self, TransferError> {
        let dma_direction = match direction {
            Direction::In => DmaDirection::FromDevice,
            Direction::Out => DmaDirection::ToDevice,
        };
        let mapping = if let Some((ptr, len)) = buff.filter(|(_, len)| *len > 0) {
            let slice = unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), len) };
            Some(
                dma.map_streaming_slice(slice, ALIGN, dma_direction)
                    .map_err(|err| TransferError::Other(anyhow!("DMA mapping failed: {err}")))?,
            )
        } else {
            None
        };

        Ok(Self {
            kind,
            direction,
            mapping,
            transfer_len: 0,
            iso_packet_actual_lengths: Vec::new(),
        })
    }

    pub(crate) fn from_request(
        dma: &Kernel,
        request: TransferRequest,
    ) -> Result<Self, TransferError> {
        let (kind, direction, buffer) = request.into();
        let buff = buffer.map(|buffer| (buffer.ptr, buffer.len));
        Self::new(dma, kind, direction, buff)
    }

    pub fn buffer_len(&self) -> usize {
        if let Some(ref mapping) = self.mapping {
            mapping.len()
        } else {
            0
        }
    }

    pub fn dma_addr(&self) -> u64 {
        if let Some(ref mapping) = self.mapping {
            mapping.dma_addr().as_u64()
        } else {
            0
        }
    }

    pub fn complete_for_cpu_all(&self) {
        if let Some(ref mapping) = self.mapping {
            mapping.complete_for_cpu_all();
        }
    }

    pub fn prepare_for_device_all(&self) {
        if let Some(ref mapping) = self.mapping {
            mapping.prepare_for_device_all();
        }
    }
}
