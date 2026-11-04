use alloc::sync::Arc;

use axpoll::IoEvents;
use axpoll_set::PollSet;
use rdif_audio::PcmError;
use syscalls::Errno;

use crate::{StarryError, StarryResult, sync::Mutex};

pub(crate) type SessionId = u64;

/// Errors crossing from the asynchronous PCM worker to a user operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SoundError {
    InvalidInput,
    Busy,
    WouldBlock,
    Xrun,
    NoDevice,
    BadState,
    TokenExhausted,
    Io,
}

impl SoundError {
    pub(crate) const fn is_fatal(self) -> bool {
        matches!(self, Self::NoDevice | Self::TokenExhausted | Self::Io)
    }

    pub(crate) const fn into_starry(self) -> StarryError {
        match self {
            Self::InvalidInput => StarryError::InvalidInput,
            Self::Busy => StarryError::ResourceBusy,
            Self::WouldBlock => StarryError::WouldBlock,
            Self::Xrun => StarryError::BrokenPipe,
            Self::NoDevice => StarryError::NoSuchDevice,
            Self::BadState => StarryError::Errno(Errno::EBADFD),
            Self::TokenExhausted => StarryError::Errno(Errno::EOVERFLOW),
            Self::Io => StarryError::Io,
        }
    }
}

impl From<PcmError> for SoundError {
    fn from(error: PcmError) -> Self {
        match error {
            PcmError::Unsupported | PcmError::InvalidConfig | PcmError::InvalidBuffer => {
                Self::InvalidInput
            }
            PcmError::BadState => Self::BadState,
            PcmError::Busy => Self::Busy,
            PcmError::Again => Self::WouldBlock,
            PcmError::TokenExhausted => Self::TokenExhausted,
            PcmError::DeadlineExpired
            | PcmError::Timeout
            | PcmError::Protocol
            | PcmError::Device
            | PcmError::NotReady
            | PcmError::ResetPending => Self::NoDevice,
        }
    }
}

/// A control operation identity used to match an interrupted retry with the
/// request already owned by the worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlKind {
    Configure,
    Prepare,
    Start,
    HwFree,
    Drop,
    Drain,
}

/// Completion published by the worker before it wakes pollers.
pub(crate) struct Request {
    result: Mutex<Option<Result<(), SoundError>>>,
    wake: Arc<PollSet>,
}

impl Request {
    pub(crate) fn new(wake: Arc<PollSet>) -> Self {
        Self {
            result: Mutex::new(None),
            wake,
        }
    }

    pub(crate) fn complete(&self, result: Result<(), SoundError>) {
        *self.result.lock() = Some(result);
        // SAFETY: the worker publishes the request result before calling this
        // from task context, and no session state lock is held here.
        unsafe {
            self.wake.wake(IoEvents::IN | IoEvents::OUT | IoEvents::ERR);
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.result.lock().is_some()
    }

    /// Returns the current result without consuming it.
    ///
    /// `poll_io` may invoke this repeatedly around readiness registration, so
    /// the operation must stay idempotent.
    pub(crate) fn poll_result(&self) -> StarryResult<()> {
        match *self.result.lock() {
            Some(Ok(())) => Ok(()),
            Some(Err(error)) => Err(error.into_starry()),
            None => Err(StarryError::WouldBlock),
        }
    }
}

/// A user control request and the extra payload needed after backend success.
pub(crate) struct ControlCommand {
    pub(crate) kind: ControlKind,
    pub(crate) request: Arc<Request>,
    pub(crate) configure: Option<(alsa_pcm_uapi::PcmFormat, rdif_audio::PcmConfig)>,
}
