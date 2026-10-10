//! Optional portable playback capability; no OS registration or scheduler glue.
use rdif_audio::{
    DriverGeneric, Playback, PlaybackConfig, PlaybackError, PlaybackToken, SampleFormat,
};

use crate::{Controller, Error, HdaIo};

const CONFIGURATIONS: [PlaybackConfig; 1] = [PlaybackConfig {
    sample_format: SampleFormat::S16Le,
    sample_rate_hz: 48_000,
    channels: 2,
    period_frames: 1024,
    period_count: 4,
}];

impl<B: HdaIo + Send + 'static> DriverGeneric for Controller<B> {
    fn name(&self) -> &str {
        "Intel HDA analog playback"
    }
}

impl<B: HdaIo + Send + 'static> Playback for Controller<B> {
    fn configurations(&self) -> &[PlaybackConfig] {
        &CONFIGURATIONS
    }
    fn max_poll_interval_ns(&self) -> u64 {
        10_000_000
    }
    fn prepare(&mut self, config: PlaybackConfig) -> Result<(), PlaybackError> {
        if config != CONFIGURATIONS[0] {
            return Err(PlaybackError::Unsupported);
        }
        Controller::prepare(
            self,
            config.period_bytes().ok_or(PlaybackError::InvalidPeriod)? as u32,
            u32::from(config.period_count),
        )
        .map_err(playback_error)
    }
    fn submit(&mut self, pcm: &[u8]) -> Result<PlaybackToken, PlaybackError> {
        Controller::submit(self, pcm)
            .map(PlaybackToken)
            .map_err(playback_error)
    }
    fn complete(&mut self) -> Result<Option<PlaybackToken>, PlaybackError> {
        Controller::complete(self)
            .map(|token| token.map(PlaybackToken))
            .map_err(playback_error)
    }
    fn release(&mut self) -> Result<(), PlaybackError> {
        Controller::release(self).map_err(playback_error)
    }
    fn abort(&mut self) -> Result<(), PlaybackError> {
        Controller::abort(self).map_err(playback_error)
    }
    fn shutdown(&mut self) -> Result<(), PlaybackError> {
        Controller::shutdown(self).map_err(playback_error)
    }
}

fn playback_error(error: Error) -> PlaybackError {
    match error {
        Error::Unsupported => PlaybackError::Unsupported,
        Error::InvalidParam => PlaybackError::InvalidPeriod,
        Error::BadState => PlaybackError::BadState,
        Error::ResourceBusy => PlaybackError::Busy,
        Error::Again => PlaybackError::Again,
        Error::Io | Error::RegisterRange | Error::Dma(_) | Error::DmaAddress => {
            PlaybackError::Device
        }
    }
}
