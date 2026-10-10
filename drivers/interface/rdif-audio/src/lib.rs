//! PCM playback with one exclusive stream owner and copied submissions.
#![no_std]

pub use rdif_base::DriverGeneric;

/// The encoding of each interleaved channel sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleFormat {
    /// Signed 16-bit little-endian samples.
    S16Le,
}

/// A complete configuration advertised by a playback device.
/// Drivers reject configurations they do not advertise, before changing state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaybackConfig {
    pub sample_format: SampleFormat,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub period_frames: u32,
    pub period_count: u16,
}

impl PlaybackConfig {
    /// Bytes in one period; zero-sized or overflowing configurations are invalid.
    pub const fn period_bytes(self) -> Option<usize> {
        if self.sample_rate_hz == 0
            || self.channels == 0
            || self.period_frames == 0
            || self.period_count == 0
        {
            return None;
        }
        let sample_bytes = match self.sample_format {
            SampleFormat::S16Le => 2usize,
        };
        match sample_bytes.checked_mul(self.channels as usize) {
            Some(frame_bytes) => frame_bytes.checked_mul(self.period_frames as usize),
            None => None,
        }
    }
}

/// Identifies one accepted submission. Unique among outstanding submissions on
/// its device, not a global ID or a capability to access DMA memory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaybackToken(pub u16);

/// Portable playback errors, independent of OS errors and controller registers.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum PlaybackError {
    #[error("unsupported PCM configuration")]
    Unsupported,
    #[error("invalid PCM period")]
    InvalidPeriod,
    #[error("playback stream is not in a usable state")]
    BadState,
    #[error("playback must be drained or cancelled first")]
    Busy,
    #[error("no submission capacity; collect completions before retrying")]
    Again,
    #[error("playback device or DMA operation failed")]
    Device,
}

/// A registered, movable playback device. All operations require one owner in
/// task context. No OS locks, scheduling, capture, mixing or user ABI are implied.
///
/// A polling owner must collect completions at the device's advertised interval;
/// this is not an IRQ completion contract. Completion means DMA consumption,
/// whereas `release` also drains the device's bounded output tail.
pub trait Playback: DriverGeneric {
    fn configurations(&self) -> &[PlaybackConfig];
    /// Maximum interval between completion polls while playback is running.
    fn max_poll_interval_ns(&self) -> u64;
    /// Prepare a supported configuration, only after drain or explicit cancel.
    fn prepare(&mut self, config: PlaybackConfig) -> Result<(), PlaybackError>;
    /// Copy exactly one period before returning success. The caller may then
    /// reuse its slice. An error accepts no new period. Playback starts when
    /// the first period is accepted; subsequent completions preserve order.
    fn submit(&mut self, pcm: &[u8]) -> Result<PlaybackToken, PlaybackError>;
    /// Return each consumed period once, or None if there is no completion yet.
    fn complete(&mut self) -> Result<Option<PlaybackToken>, PlaybackError>;
    /// Drain the output tail and stop. Busy leaves pending work owned by the
    /// device; callers must continue collecting completions or explicitly abort.
    /// Already completed but uncollected tokens remain available after release;
    /// collect them before preparing another stream.
    fn release(&mut self) -> Result<(), PlaybackError>;
    /// Stop and cancel outstanding tokens. On error resources remain owned or
    /// quarantined, never released on a timeout alone. Re-prepare after success.
    fn abort(&mut self) -> Result<(), PlaybackError>;
    /// Permanently stop the device. Dropping must also stop or quarantine DMA.
    /// A failed shutdown does not authorize freeing hardware-visible resources.
    fn shutdown(&mut self) -> Result<(), PlaybackError>;
}
