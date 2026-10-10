//! Registered PCM playback capabilities, transferred to one task-context owner.
use alloc::{boxed::Box, string::String, vec::Vec};

pub use rdif_audio::{Playback, PlaybackConfig, PlaybackError, PlaybackToken, SampleFormat};
use rdrive::DriverGeneric;

use crate::{
    BindingInfo, Error,
    registration::{BoundDevice, TakeRegistered, register_bound_device, take_registered_device},
};

#[cfg(feature = "intel-hda")]
mod hda;

/// Registry metadata, not a second playback owner. Taking the capability moves
/// it out exactly once; the existing rdrive registry owns discovery and locking.
pub struct PlatformPlaybackDevice {
    name: String,
    device: Option<Box<dyn Playback>>,
    info: BindingInfo,
}

impl DriverGeneric for PlatformPlaybackDevice {
    fn name(&self) -> &str {
        &self.name
    }
}
impl BoundDevice for PlatformPlaybackDevice {
    fn binding_info(&self) -> &BindingInfo {
        &self.info
    }
}
impl TakeRegistered for PlatformPlaybackDevice {
    type Output = Box<dyn Playback>;
    fn take_registered(&mut self) -> Option<Self::Output> {
        self.device.take()
    }
}

/// Register a polling playback device. It must not enable an unowned IRQ source.
pub trait PlatformDevicePlayback {
    fn register_playback<T: Playback + 'static>(self, device: T);
}
impl PlatformDevicePlayback for rdrive::PlatformDevice {
    fn register_playback<T: Playback + 'static>(self, device: T) {
        let name = device.name().into();
        register_bound_device(
            self,
            PlatformPlaybackDevice {
                name,
                device: Some(Box::new(device)),
                info: BindingInfo::empty(),
            },
        );
    }
}

/// Move discovered playback devices out of the registry exactly once. The
/// caller must serialize discovery/ownership transfer as for GPU/input devices.
pub fn take_playback_devices() -> crate::Result<Vec<Box<dyn Playback>>> {
    let mut devices = Vec::new();
    for device in rdrive::get_list::<PlatformPlaybackDevice>() {
        devices.push(take_registered_device(device).ok_or(Error::DeviceUnavailable)?);
    }
    Ok(devices)
}
