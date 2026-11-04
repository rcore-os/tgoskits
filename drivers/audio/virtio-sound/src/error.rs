//! Typed construction and protocol errors.

use dma_api::DmaError;
use rdif_audio::PcmError;
use virtio_drivers::Error as VirtioError;

/// A construction or internal VirtIO Sound error.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("transport is not a VirtIO Sound device")]
    NotSoundDevice,
    #[error("device feature or capability is not supported")]
    Unsupported,
    #[error("PCM configuration is invalid")]
    InvalidConfig,
    #[error("operation is not valid in the current state")]
    BadState,
    #[error("a control request is already in flight")]
    Busy,
    #[error("queue has no capacity")]
    Again,
    #[error("caller deadline has expired")]
    DeadlineExpired,
    #[error("device response deadline elapsed")]
    Timeout,
    #[error("device response violated the VirtIO Sound protocol")]
    Protocol,
    #[error("device stop is not yet confirmed")]
    ResetPending,
    #[error("card identity space is exhausted")]
    CardIdExhausted,
    #[error("PCM operation failed: {0}")]
    Pcm(#[from] PcmError),
    #[error("DMA operation failed: {0}")]
    Dma(#[from] DmaError),
    #[error("VirtIO operation failed: {0}")]
    Virtio(#[from] VirtioError),
}

impl Error {
    pub(crate) fn as_pcm(&self) -> PcmError {
        match self {
            Self::NotSoundDevice | Self::Unsupported => PcmError::Unsupported,
            Self::InvalidConfig => PcmError::InvalidConfig,
            Self::BadState => PcmError::BadState,
            Self::Busy => PcmError::Busy,
            Self::Again => PcmError::Again,
            Self::DeadlineExpired => PcmError::DeadlineExpired,
            Self::Timeout => PcmError::Timeout,
            Self::Protocol => PcmError::Protocol,
            Self::ResetPending => PcmError::ResetPending,
            Self::CardIdExhausted => PcmError::Device,
            Self::Pcm(error) => *error,
            Self::Dma(_) | Self::Virtio(_) => PcmError::Device,
        }
    }
}

pub(crate) fn virtio_pcm_error(error: VirtioError) -> PcmError {
    match error {
        VirtioError::QueueFull | VirtioError::NotReady => PcmError::Again,
        VirtioError::WrongToken
        | VirtioError::AlreadyUsed
        | VirtioError::InvalidParam
        | VirtioError::IoError
        | VirtioError::ConfigSpaceTooSmall
        | VirtioError::ConfigSpaceMissing => PcmError::Protocol,
        VirtioError::DmaError => PcmError::Device,
        VirtioError::Unsupported => PcmError::Unsupported,
        VirtioError::SocketDeviceError(_) => PcmError::Device,
    }
}
