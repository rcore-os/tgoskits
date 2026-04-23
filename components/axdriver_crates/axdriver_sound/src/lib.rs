//! Common traits and types for sound device drivers.

#![no_std]

#[doc(no_inline)]
pub use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};

/// Stream direction.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SoundDirection {
    /// Playback stream (guest -> device).
    Playback,
    /// Capture stream (device -> guest).
    Capture,
}

/// PCM sample format.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SoundFormat {
    S16LE,
    S24LE,
    S32LE,
    FloatLE,
}

/// Runtime stream parameters.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SoundParams {
    pub direction: SoundDirection,
    pub channels: u8,
    pub sample_rate: u32,
    pub format: SoundFormat,
    pub period_frames: u32,
    pub buffer_frames: u32,
}

/// Driver stream state.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SoundStreamState {
    Closed,
    Open,
    Prepared,
    Running,
    Stopped,
    XRun,
}

/// Basic sound capability information.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SoundCaps {
    pub playback: bool,
    pub capture: bool,
    pub min_channels: u8,
    pub max_channels: u8,
    pub min_sample_rate: u32,
    pub max_sample_rate: u32,
}

/// Operations that require a sound device driver to implement.
pub trait SoundDriverOps: BaseDriverOps {
    /// Queries sound capabilities.
    fn query_caps(&self) -> DevResult<SoundCaps>;

    /// Sets stream parameters.
    ///
    /// Returns `Err(DevError::Unsupported)` if any requested parameter is not
    /// supported by the device.
    fn set_params(&mut self, params: SoundParams) -> DevResult;

    /// Returns current stream state.
    fn stream_state(&self, direction: SoundDirection) -> DevResult<SoundStreamState>;

    /// Starts one stream direction.
    fn start(&mut self, direction: SoundDirection) -> DevResult;

    /// Stops one stream direction.
    fn stop(&mut self, direction: SoundDirection) -> DevResult;

    /// Writes PCM frames to playback stream.
    ///
    /// Returns `Err(DevError::Again)` if the device cannot accept data for now.
    fn write_frames(&mut self, data: &[u8]) -> DevResult<usize>;
}
