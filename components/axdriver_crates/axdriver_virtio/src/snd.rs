use core::marker::PhantomData;

use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use ax_driver_sound::{
    SoundCaps, SoundDirection, SoundDriverOps, SoundParams, SoundStreamState,
};
use virtio_drivers::{transport::Transport, Hal};

/// The VirtIO sound device driver (MVP skeleton).
///
/// Current stage only wires feature/dependency/interfaces for follow-up
/// implementation. Runtime probing/stream queues are not enabled yet.
pub struct VirtIoSndDev<H: Hal, T: Transport> {
    _phantom: PhantomData<(H, T)>,
}

unsafe impl<H: Hal, T: Transport> Send for VirtIoSndDev<H, T> {}
unsafe impl<H: Hal, T: Transport> Sync for VirtIoSndDev<H, T> {}

impl<H: Hal, T: Transport> VirtIoSndDev<H, T> {
    /// Creates a new sound driver instance.
    ///
    /// The current MVP stage keeps this constructor as a placeholder and
    /// returns `DevError::Unsupported` until virtio-snd transport support is
    /// integrated.
    pub fn try_new(_transport: T) -> DevResult<Self> {
        Err(DevError::Unsupported)
    }
}

impl<H: Hal, T: Transport> BaseDriverOps for VirtIoSndDev<H, T> {
    fn device_name(&self) -> &str {
        "virtio-snd"
    }

    fn device_type(&self) -> DeviceType {
        // `ax-driver-base` has no dedicated Sound variant yet.
        // Keep `Char` here as a temporary category until base device taxonomy
        // is extended in a follow-up PR.
        DeviceType::Char
    }
}

impl<H: Hal, T: Transport> SoundDriverOps for VirtIoSndDev<H, T> {
    fn query_caps(&self) -> DevResult<SoundCaps> {
        Err(DevError::Unsupported)
    }

    fn set_params(&mut self, _params: SoundParams) -> DevResult {
        Err(DevError::Unsupported)
    }

    fn stream_state(&self, _direction: SoundDirection) -> DevResult<SoundStreamState> {
        Err(DevError::Unsupported)
    }

    fn start(&mut self, _direction: SoundDirection) -> DevResult {
        Err(DevError::Unsupported)
    }

    fn stop(&mut self, _direction: SoundDirection) -> DevResult {
        Err(DevError::Unsupported)
    }

    fn write_frames(&mut self, _data: &[u8]) -> DevResult<usize> {
        Err(DevError::Unsupported)
    }
}
