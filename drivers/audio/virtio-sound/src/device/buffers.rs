//! Bounded DMA pools and per-request metadata.

use core::{array, num::NonZeroUsize};

use dma_api::{CpuDmaBuffer, DeviceDma, DmaDirection, InFlightDma};
use rdif_audio::{MAX_PERIOD_COUNT, PcmConfig, PcmDirection, PcmIoToken};

use super::session::StreamSession;
use crate::{
    Error,
    wire::{PCM_STATUS_SIZE, STREAM_ID_SIZE},
};

pub(super) const MAX_PERIODS: usize = MAX_PERIOD_COUNT as usize;
pub(super) const PCM_BUFFER_ALIGN: usize = 64;
pub(super) const STATUS_BUFFER_ALIGN: usize = 8;

pub(super) struct DirectionBuffers {
    pub(super) data: [Option<CpuDmaBuffer>; MAX_PERIODS],
    pub(super) headers: [Option<CpuDmaBuffer>; MAX_PERIODS],
    pub(super) statuses: [Option<CpuDmaBuffer>; MAX_PERIODS],
}

impl DirectionBuffers {
    pub(super) fn empty() -> Self {
        Self {
            data: array::from_fn(|_| None),
            headers: array::from_fn(|_| None),
            statuses: array::from_fn(|_| None),
        }
    }

    pub(super) fn allocate(
        dma: &DeviceDma,
        config: PcmConfig,
        stream_id: u32,
        direction: PcmDirection,
    ) -> Result<Self, Error> {
        if usize::from(config.period_count) > MAX_PERIODS {
            return Err(Error::InvalidConfig);
        }
        let period_bytes = config.period_bytes().ok_or(Error::InvalidConfig)?;
        let period_len = NonZeroUsize::new(period_bytes).ok_or(Error::InvalidConfig)?;
        let status_len = NonZeroUsize::new(PCM_STATUS_SIZE).ok_or(Error::InvalidConfig)?;
        let header_len = NonZeroUsize::new(STREAM_ID_SIZE).ok_or(Error::InvalidConfig)?;
        let data_direction = match direction {
            PcmDirection::Playback => DmaDirection::ToDevice,
            PcmDirection::Capture => DmaDirection::FromDevice,
        };
        let mut buffers = Self::empty();
        let count = usize::from(config.period_count);
        for (data_slot, (header_slot, status_slot)) in buffers
            .data
            .iter_mut()
            .zip(buffers.headers.iter_mut().zip(buffers.statuses.iter_mut()))
            .take(count)
        {
            *data_slot = Some(CpuDmaBuffer::new_zero(
                dma,
                period_len,
                PCM_BUFFER_ALIGN,
                data_direction,
            )?);
            let mut header = CpuDmaBuffer::new_zero(
                dma,
                header_len,
                STATUS_BUFFER_ALIGN,
                DmaDirection::ToDevice,
            )?;
            header.copy_from_slice_cpu(&stream_id.to_le_bytes());
            *header_slot = Some(header);
            *status_slot = Some(CpuDmaBuffer::new_zero(
                dma,
                status_len,
                STATUS_BUFFER_ALIGN,
                DmaDirection::FromDevice,
            )?);
        }
        Ok(buffers)
    }
}

pub(super) struct EventInFlight {
    pub(super) queue_token: u16,
    pub(super) pool_index: usize,
    pub(super) buffer: InFlightDma,
}

pub(super) struct TxInFlight {
    pub(super) queue_token: u16,
    pub(super) token: PcmIoToken,
    pub(super) pool_index: usize,
    pub(super) frames: u32,
    pub(super) deadline_ns: u64,
    pub(super) data: InFlightDma,
    pub(super) header: InFlightDma,
    pub(super) status: InFlightDma,
}

pub(super) struct RxInFlight {
    pub(super) queue_token: u16,
    pub(super) token: PcmIoToken,
    pub(super) pool_index: usize,
    pub(super) deadline_ns: u64,
    pub(super) data: InFlightDma,
    pub(super) header: InFlightDma,
    pub(super) status: InFlightDma,
}

pub(super) fn find_complete_slot(buffers: &DirectionBuffers) -> Option<usize> {
    buffers.data.iter().enumerate().find_map(|(index, data)| {
        (data.is_some() && buffers.headers[index].is_some() && buffers.statuses[index].is_some())
            .then_some(index)
    })
}

pub(super) fn find_rx_slot(session: &StreamSession) -> Option<usize> {
    session
        .buffers
        .data
        .iter()
        .enumerate()
        .find_map(|(index, data)| {
            (data.is_some()
                && session.buffers.headers[index].is_some()
                && session.buffers.statuses[index].is_some())
            .then_some(index)
        })
}

pub(super) fn find_rx_recycle_slot(buffers: &DirectionBuffers) -> Option<usize> {
    buffers.data.iter().enumerate().find_map(|(index, data)| {
        (data.is_none() && buffers.headers[index].is_some() && buffers.statuses[index].is_some())
            .then_some(index)
    })
}
