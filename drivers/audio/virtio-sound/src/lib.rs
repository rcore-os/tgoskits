//! Portable VirtIO Sound PCM core with explicit, non-blocking service.
//!
//! The card owns the control, event, TX and RX VirtIO queues. OS adapters
//! provide a [`virtio_drivers::Hal`] for queue memory and a
//! [`dma_api::DeviceDma`] for PCM buffers. Both capabilities must describe the
//! same device address domain.
#![no_std]

mod device;
mod error;
mod wire;

pub use device::VirtioSound;
pub use error::Error;
pub use rdif_audio::{
    MAX_PERIOD_COUNT, MAX_PERIOD_FRAMES, MIN_PERIOD_COUNT, MIN_PERIOD_FRAMES,
    PERIOD_FRAME_ALIGNMENT, PcmBuffer, PcmBufferOwner, PcmCapabilities, PcmCard, PcmCardState,
    PcmCompletion, PcmConfig, PcmControlOp, PcmControlToken, PcmDirection, PcmError, PcmEvent,
    PcmEventKind, PcmIoToken, PcmServiceBudget, PcmServiceReport, PcmStream, PcmStreamCapabilities,
    SampleFormat,
};
