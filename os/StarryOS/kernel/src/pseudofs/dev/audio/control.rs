use alloc::{borrow::Cow, sync::Arc};

use alsa_pcm_uapi::{
    PcmInfo, SNDRV_CTL_IOCTL_CARD_INFO, SNDRV_CTL_IOCTL_PCM_INFO, SNDRV_CTL_IOCTL_PCM_NEXT_DEVICE,
    SNDRV_CTL_IOCTL_PCM_PREFER_SUBDEVICE, SNDRV_CTL_IOCTL_PVERSION, SNDRV_CTL_VERSION,
    SNDRV_PCM_STREAM_CAPTURE, SNDRV_PCM_STREAM_PLAYBACK,
};
use axpoll::{IoEvents, Pollable};
use rdif_audio::PcmDirection;
use zerocopy::IntoBytes;

use crate::{
    StarryError, StarryResult,
    file::{File as KernelFile, FileLike, IoDst, IoSrc, Kstat},
    mm::UserPtr,
    sound::{SoundService, config},
    task::UserTaskRef,
};

pub(super) struct ControlFile {
    base: KernelFile,
    service: Arc<SoundService>,
}

impl ControlFile {
    pub(super) fn new(base: KernelFile, service: Arc<SoundService>) -> Self {
        Self { base, service }
    }
}

impl FileLike for ControlFile {
    fn validate_write_access(&self) -> StarryResult {
        self.base.validate_write_access()
    }

    fn read(&self, _dst: &mut IoDst) -> StarryResult<usize> {
        Err(StarryError::InvalidInput)
    }

    fn write(&self, _src: &mut IoSrc) -> StarryResult<usize> {
        Err(StarryError::InvalidInput)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        self.base.stat()
    }

    fn path(&self) -> Cow<'_, str> {
        self.base.path()
    }

    fn ioctl(&self, current: &UserTaskRef, cmd: u32, arg: usize) -> StarryResult<usize> {
        match cmd {
            SNDRV_CTL_IOCTL_PVERSION => {
                UserPtr::<i32>::from(arg).write(current, SNDRV_CTL_VERSION as i32)?;
            }
            SNDRV_CTL_IOCTL_CARD_INFO => {
                let info = config::ctl_card_info();
                UserPtr::<u8>::from(arg).write_slice(current, info.as_bytes())?;
            }
            SNDRV_CTL_IOCTL_PCM_NEXT_DEVICE => {
                let device = UserPtr::<i32>::from(arg).read(current)?;
                UserPtr::<i32>::from(arg).write(current, if device < 0 { 0 } else { -1 })?;
            }
            SNDRV_CTL_IOCTL_PCM_INFO => {
                // SAFETY: `PcmInfo` consists only of integer and byte-array
                // fields, so every initialized userspace bit pattern is valid.
                let request = unsafe { UserPtr::<PcmInfo>::from(arg).read_abi(current)? };
                if request.device != 0 {
                    return Err(StarryError::NoSuchDevice);
                }
                let direction = match request.stream {
                    SNDRV_PCM_STREAM_PLAYBACK => PcmDirection::Playback,
                    SNDRV_PCM_STREAM_CAPTURE => PcmDirection::Capture,
                    _ => return Err(StarryError::InvalidInput),
                };
                let card = self
                    .service
                    .publication()
                    .ok_or(StarryError::NoSuchDevice)?;
                let present = match direction {
                    PcmDirection::Playback => card.playback.is_some(),
                    PcmDirection::Capture => card.capture.is_some(),
                };
                if !present {
                    return Err(StarryError::NoSuchDevice);
                }
                let info = config::pcm_info(
                    direction,
                    request.stream,
                    if self.service.is_busy(direction) {
                        0
                    } else {
                        1
                    },
                );
                UserPtr::<u8>::from(arg).write_slice(current, info.as_bytes())?;
            }
            SNDRV_CTL_IOCTL_PCM_PREFER_SUBDEVICE => {
                let subdevice = UserPtr::<i32>::from(arg).read(current)?;
                if !matches!(subdevice, -1 | 0) {
                    return Err(StarryError::NoSuchDevice);
                }
            }
            _ => return Err(StarryError::NotATty),
        }
        Ok(0)
    }

    fn open_flags(&self) -> u32 {
        self.base.open_flags()
    }

    fn nonblocking(&self) -> bool {
        self.base.nonblocking()
    }

    fn set_nonblocking(&self, value: bool) -> StarryResult {
        self.base.set_nonblocking(value)
    }
}

impl Pollable for ControlFile {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }

    unsafe fn register_exclusive(
        &self,
        _sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        _events: IoEvents,
    ) {
    }
}
