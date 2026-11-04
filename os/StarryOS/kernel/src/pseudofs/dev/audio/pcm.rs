use alloc::{borrow::Cow, sync::Arc, vec::Vec};

use alsa_pcm_uapi::{
    PcmHwParams, PcmSwParams, PcmSyncPtr, PcmXferi, SNDRV_PCM_IOCTL_DELAY, SNDRV_PCM_IOCTL_DRAIN,
    SNDRV_PCM_IOCTL_DROP, SNDRV_PCM_IOCTL_HW_FREE, SNDRV_PCM_IOCTL_HW_PARAMS,
    SNDRV_PCM_IOCTL_HW_REFINE, SNDRV_PCM_IOCTL_HWSYNC, SNDRV_PCM_IOCTL_INFO,
    SNDRV_PCM_IOCTL_PREPARE, SNDRV_PCM_IOCTL_PVERSION, SNDRV_PCM_IOCTL_READI_FRAMES,
    SNDRV_PCM_IOCTL_START, SNDRV_PCM_IOCTL_STATUS, SNDRV_PCM_IOCTL_STATUS_EXT,
    SNDRV_PCM_IOCTL_SW_PARAMS, SNDRV_PCM_IOCTL_SYNC_PTR, SNDRV_PCM_IOCTL_TSTAMP,
    SNDRV_PCM_IOCTL_TTSTAMP, SNDRV_PCM_IOCTL_USER_PVERSION, SNDRV_PCM_IOCTL_WRITEI_FRAMES,
    SNDRV_PCM_IOCTL_XRUN, SNDRV_PCM_VERSION, accept_hw_params,
};
use ax_io::prelude::*;
use axpoll::{IoEvents, Pollable};
use rdif_audio::{PcmConfig, PcmDirection, SampleFormat};
use syscalls::Errno;
use zerocopy::IntoBytes;

use crate::{
    StarryError, StarryResult,
    file::{File as KernelFile, FileLike, IoDst, IoSrc, Kstat},
    mm::{UserConstPtr, UserPtr},
    pseudofs::DeviceMmap,
    sound::{
        SoundError, config,
        protocol::{ControlCommand, ControlKind, Request},
        session::{PcmSession, StreamPhase},
    },
    sync::Mutex,
    task::{
        UserTaskRef,
        future::{block_on_user, poll_io},
    },
};

struct PendingControl {
    kind: ControlKind,
    request: Arc<Request>,
    hw_params: Option<PcmHwParams>,
}

struct PlaybackTransfer {
    frames: u32,
    bytes: usize,
}

const SNDRV_PCM_MMAP_OFFSET_STATUS_OLD: u64 = 0x8000_0000;
const SNDRV_PCM_MMAP_OFFSET_CONTROL_OLD: u64 = 0x8100_0000;
const SNDRV_PCM_MMAP_OFFSET_STATUS_NEW: u64 = 0x8200_0000;
const SNDRV_PCM_MMAP_OFFSET_CONTROL_NEW: u64 = 0x8300_0000;

/// One open-file description of an ALSA hardware PCM node.
pub(crate) struct PcmFile {
    base: KernelFile,
    session: Arc<PcmSession>,
    pending: Mutex<Option<Arc<PendingControl>>>,
}

impl PcmFile {
    pub(crate) fn open(base: KernelFile, session: Arc<PcmSession>) -> Self {
        Self {
            base,
            session,
            pending: Mutex::new(None),
        }
    }

    fn run_control(
        &self,
        current: &UserTaskRef,
        kind: ControlKind,
        hw_params: Option<PcmHwParams>,
        submit: impl FnOnce(Arc<Request>) -> StarryResult<()>,
    ) -> StarryResult<Option<PcmHwParams>> {
        let (pending, created) = self.prepare_pending(kind, hw_params)?;
        if created && let Err(error) = submit(pending.request.clone()) {
            self.clear_pending(&pending);
            return Err(error);
        }
        debug_assert_ne!(kind, ControlKind::Drain);
        let result = block_on_user(
            current,
            poll_io(self, IoEvents::IN | IoEvents::ERR, false, || {
                // A backend EAGAIN is a final result, not another reason
                // to sleep. Wait for completion publication itself, then
                // interpret the stored result once.
                pending
                    .request
                    .is_done()
                    .then_some(())
                    .ok_or(StarryError::WouldBlock)
            }),
        )
        .into_result()?;
        result?;
        let hw_output = pending.hw_params;
        self.clear_pending(&pending);
        pending.request.poll_result()?;
        Ok(hw_output)
    }

    fn run_drain(
        &self,
        current: &UserTaskRef,
        submit: impl FnOnce(Arc<Request>) -> StarryResult<()>,
    ) -> StarryResult<()> {
        let non_blocking = self.nonblocking();
        let (pending, created) = self.prepare_pending(ControlKind::Drain, None)?;
        if created && let Err(error) = submit(pending.request.clone()) {
            self.clear_pending(&pending);
            return Err(error);
        }

        // Linux publishes drain-init state before returning EAGAIN for a
        // nonblocking drain. Wait for that short initialization regardless of
        // the file status flag, then leave actual playback draining to the
        // worker.
        let initialized = block_on_user(
            current,
            poll_io(self, IoEvents::IN | IoEvents::ERR, false, || {
                pending
                    .request
                    .is_done()
                    .then_some(())
                    .ok_or(StarryError::WouldBlock)
            }),
        )
        .into_result()?;
        initialized?;
        let init_result = pending.request.poll_result();
        self.clear_pending(&pending);
        init_result?;

        if non_blocking {
            return Err(StarryError::WouldBlock);
        }
        if self.session.direction() != PcmDirection::Playback {
            return Ok(());
        }

        // A blocking playback drain waits only for the worker-owned drain to
        // stop the stream. Capture returns after its stop initialization even
        // when unread tail frames remain.
        block_on_user(
            current,
            poll_io(
                self,
                IoEvents::IN | IoEvents::OUT | IoEvents::ERR,
                false,
                || match self.session.phase() {
                    StreamPhase::Draining => Err(StarryError::WouldBlock),
                    StreamPhase::Failed => Err(StarryError::NoSuchDevice),
                    _ => Ok(()),
                },
            ),
        )
        .into_result()?
    }

    fn prepare_pending(
        &self,
        kind: ControlKind,
        hw_params: Option<PcmHwParams>,
    ) -> StarryResult<(Arc<PendingControl>, bool)> {
        let mut slot = self.pending.lock();
        if let Some(pending) = slot.as_ref() {
            if pending.kind == kind {
                return Ok((pending.clone(), false));
            }
            // Linux terminal controls may stop an in-progress drain. The
            // worker still owns the abandoned request and its DMA; this slot
            // only tracks the next user-visible operation.
            let can_supersede = pending.kind == ControlKind::Drain
                && matches!(
                    kind,
                    ControlKind::Drop | ControlKind::Prepare | ControlKind::HwFree
                );
            if !pending.request.is_done() && !can_supersede {
                return Err(StarryError::ResourceBusy);
            }
        }
        let pending = Arc::new(PendingControl {
            kind,
            request: Arc::new(Request::new(self.session.poll_source())),
            hw_params,
        });
        *slot = Some(pending.clone());
        Ok((pending, true))
    }

    fn clear_pending(&self, pending: &Arc<PendingControl>) {
        let mut slot = self.pending.lock();
        if slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, pending))
        {
            *slot = None;
        }
    }

    fn frame_bytes(&self) -> StarryResult<usize> {
        self.session
            .frame_bytes()
            .map(|value| value as usize)
            .ok_or_else(|| SoundError::BadState.into_starry())
    }

    fn max_transfer_frames(&self, requested: u64) -> StarryResult<usize> {
        let buffer = self
            .session
            .buffer_frames()
            .ok_or_else(|| SoundError::BadState.into_starry())? as usize;
        let requested = usize::try_from(requested).map_err(|_| StarryError::InvalidInput)?;
        Ok(requested.min(buffer))
    }

    fn wait_playback_ready(&self, current: &UserTaskRef) -> StarryResult<()> {
        block_on_user(
            current,
            poll_io(
                self,
                IoEvents::OUT | IoEvents::ERR,
                self.nonblocking(),
                || match self
                    .session
                    .playback_writable()
                    .map_err(SoundError::into_starry)?
                {
                    true => Ok(()),
                    false => Err(StarryError::WouldBlock),
                },
            ),
        )
        .into_result()?
    }

    fn transfer_playback(
        &self,
        current: &UserTaskRef,
        data: &[u8],
    ) -> StarryResult<PlaybackTransfer> {
        loop {
            self.wait_playback_ready(current)?;
            let transfer = self.session.lock_io();
            let frame_bytes = self.frame_bytes()?;
            if !data.len().is_multiple_of(frame_bytes) {
                return Err(StarryError::InvalidInput);
            }
            match self.session.try_write(data) {
                Ok(frames) => {
                    let frames = u32::try_from(frames).map_err(|_| StarryError::InvalidInput)?;
                    let bytes = (frames as usize)
                        .checked_mul(frame_bytes)
                        .ok_or(StarryError::InvalidInput)?;
                    drop(transfer);
                    self.session.notify_worker();
                    self.session.notify();
                    return Ok(PlaybackTransfer { frames, bytes });
                }
                Err(SoundError::WouldBlock) => {
                    drop(transfer);
                    if self.nonblocking() {
                        return Err(StarryError::WouldBlock);
                    }
                }
                Err(error) => return Err(error.into_starry()),
            }
        }
    }

    fn wait_capture_ready(&self, current: &UserTaskRef) -> StarryResult<()> {
        block_on_user(
            current,
            poll_io(
                self,
                IoEvents::IN | IoEvents::ERR,
                self.nonblocking(),
                || {
                    self.session.request_capture_start();
                    match self
                        .session
                        .capture_read_ready()
                        .map_err(SoundError::into_starry)?
                    {
                        true => Ok(()),
                        false => Err(StarryError::WouldBlock),
                    }
                },
            ),
        )
        .into_result()?
    }

    fn transfer_capture(
        &self,
        current: &UserTaskRef,
        available: usize,
        mut copy: impl FnMut(&[u8]) -> StarryResult<usize>,
    ) -> StarryResult<(usize, u32)> {
        loop {
            self.wait_capture_ready(current)?;
            let transfer = self.session.lock_io();
            let frame_bytes = self.frame_bytes()?;
            if !available.is_multiple_of(frame_bytes) {
                return Err(StarryError::InvalidInput);
            }
            let max_bytes = available.min(
                (self.session.buffer_frames().unwrap_or(0) as usize).saturating_mul(frame_bytes),
            );
            let Some((bytes, _)) = self
                .session
                .peek_capture(max_bytes)
                .map_err(SoundError::into_starry)?
            else {
                drop(transfer);
                if self.nonblocking() {
                    return Err(StarryError::WouldBlock);
                }
                continue;
            };
            let written = copy(&bytes)?;
            if !written.is_multiple_of(frame_bytes) {
                return Err(StarryError::InvalidInput);
            }
            let complete_frames = written / frame_bytes;
            if complete_frames == 0 {
                return Ok((0, 0));
            }
            self.session
                .commit_capture_read(complete_frames as u32)
                .map_err(SoundError::into_starry)?;
            drop(transfer);
            self.session.notify();
            return Ok((complete_frames * frame_bytes, complete_frames as u32));
        }
    }

    fn writei(&self, current: &UserTaskRef, request: PcmXferi) -> StarryResult<PcmXferi> {
        if self.session.direction() != PcmDirection::Playback {
            return Err(StarryError::InvalidInput);
        }
        if request.frames == 0 {
            let mut result = request;
            result.result = 0;
            return Ok(result);
        }
        if request.buf == 0 {
            return Err(StarryError::BadAddress);
        }
        let frame_bytes = self.frame_bytes()?;
        let frames = self.max_transfer_frames(request.frames)?;
        if frames == 0 {
            return Err(StarryError::InvalidInput);
        }
        let len = frames
            .checked_mul(frame_bytes)
            .ok_or(StarryError::InvalidInput)?;
        let data = UserConstPtr::<u8>::from(request.buf as usize).read_slice(current, len)?;
        let transferred = self.transfer_playback(current, &data)?;
        let mut result = request;
        result.result = i64::from(transferred.frames);
        Ok(result)
    }

    fn readi(&self, current: &UserTaskRef, request: PcmXferi) -> StarryResult<PcmXferi> {
        if self.session.direction() != PcmDirection::Capture {
            return Err(StarryError::InvalidInput);
        }
        if request.frames == 0 {
            let mut result = request;
            result.result = 0;
            return Ok(result);
        }
        if request.buf == 0 {
            return Err(StarryError::BadAddress);
        }
        let frame_bytes = self.frame_bytes()?;
        let frames = self.max_transfer_frames(request.frames)?;
        if frames == 0 {
            return Err(StarryError::InvalidInput);
        }
        let max_bytes = frames
            .checked_mul(frame_bytes)
            .ok_or(StarryError::InvalidInput)?;
        let (_, actual_frames) = self.transfer_capture(current, max_bytes, |bytes| {
            UserPtr::<u8>::from(request.buf as usize)
                .write_slice(current, bytes)
                .map(|()| bytes.len())
        })?;
        let mut result = request;
        result.result = i64::from(actual_frames);
        Ok(result)
    }

    fn configure(
        &self,
        current: &UserTaskRef,
        hw_params: PcmHwParams,
    ) -> StarryResult<PcmHwParams> {
        self.session
            .ensure_configurable()
            .map_err(SoundError::into_starry)?;
        let (refined, format) = accept_hw_params(&hw_params, &self.session.profile().constraints)
            .map_err(|_| StarryError::InvalidInput)?;
        let refined = expose_hw_info(refined);
        let channels = u16::try_from(format.channels).map_err(|_| StarryError::InvalidInput)?;
        let period_count =
            u16::try_from(format.period_count).map_err(|_| StarryError::InvalidInput)?;
        let config = PcmConfig {
            sample_format: SampleFormat::S16Le,
            sample_rate_hz: format.rate_hz,
            channels,
            period_frames: format.period_frames,
            period_count,
        };
        let output =
            self.run_control(current, ControlKind::Configure, Some(refined), |request| {
                Arc::clone(&self.session).submit_control(ControlCommand {
                    kind: ControlKind::Configure,
                    request,
                    configure: Some((format, config)),
                })
            })?;
        output.ok_or(StarryError::BadState)
    }
}

impl FileLike for PcmFile {
    fn validate_write_access(&self) -> StarryResult {
        self.base.validate_write_access()
    }

    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        if self.session.direction() != PcmDirection::Capture {
            return Err(StarryError::InvalidInput);
        }
        let current = crate::task::current_user_task();
        let frame_bytes = self.frame_bytes()?;
        let available = dst.remaining_mut();
        if available == 0 {
            return Ok(0);
        }
        if !available.is_multiple_of(frame_bytes) {
            return Err(StarryError::InvalidInput);
        }
        let (written, _) = self.transfer_capture(&current, available, |bytes| {
            dst.write(bytes).map_err(StarryError::from)
        })?;
        Ok(written)
    }

    fn write(&self, src: &mut IoSrc) -> StarryResult<usize> {
        if self.session.direction() != PcmDirection::Playback {
            return Err(StarryError::InvalidInput);
        }
        let current = crate::task::current_user_task();
        let frame_bytes = self.frame_bytes()?;
        let available = src.remaining();
        if available == 0 {
            return Ok(0);
        }
        if !available.is_multiple_of(frame_bytes) {
            return Err(StarryError::InvalidInput);
        }
        let bytes_to_read = available
            .min((self.session.buffer_frames().unwrap_or(0) as usize).saturating_mul(frame_bytes));
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(bytes_to_read)
            .map_err(|_| StarryError::NoMemory)?;
        bytes.resize(bytes_to_read, 0);
        let mut read = 0;
        while read < bytes.len() {
            let count = src.read(&mut bytes[read..])?;
            if count == 0 {
                break;
            }
            read += count;
        }
        if read == 0 {
            return Ok(0);
        }
        if !read.is_multiple_of(frame_bytes) {
            return Err(StarryError::InvalidInput);
        }
        self.transfer_playback(&current, &bytes[..read])
            .map(|transfer| transfer.bytes)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        self.base.stat()
    }

    fn path(&self) -> Cow<'_, str> {
        self.base.path()
    }

    fn ioctl(&self, current: &UserTaskRef, cmd: u32, arg: usize) -> StarryResult<usize> {
        match cmd {
            SNDRV_PCM_IOCTL_PVERSION => {
                UserPtr::<i32>::from(arg).write(current, SNDRV_PCM_VERSION as i32)?;
            }
            SNDRV_PCM_IOCTL_INFO => {
                let direction = self.session.direction();
                let stream = match direction {
                    PcmDirection::Playback => alsa_pcm_uapi::SNDRV_PCM_STREAM_PLAYBACK,
                    PcmDirection::Capture => alsa_pcm_uapi::SNDRV_PCM_STREAM_CAPTURE,
                };
                let info = config::pcm_info(direction, stream, 1);
                UserPtr::<u8>::from(arg).write_slice(current, info.as_bytes())?;
            }
            SNDRV_PCM_IOCTL_USER_PVERSION => {
                // Linux records this per open file for mmap compatibility.
                // This runtime rejects PCM mmap, so no current x86_64 path
                // consumes the value; preserve get_user EFAULT semantics.
                let _user_version = UserPtr::<u32>::from(arg).read(current)?;
            }
            SNDRV_PCM_IOCTL_TTSTAMP => {
                let tstamp_type = UserPtr::<i32>::from(arg).read(current)?;
                self.session
                    .set_tstamp_type(tstamp_type)
                    .map_err(SoundError::into_starry)?;
            }
            SNDRV_PCM_IOCTL_TSTAMP => {}
            SNDRV_PCM_IOCTL_HW_REFINE => {
                // SAFETY: all fields are integer wire types and every bit
                // pattern is valid for the copied structure.
                let params = unsafe { UserPtr::<PcmHwParams>::from(arg).read_abi(current)? };
                let refined = params
                    .refine(&self.session.profile().constraints)
                    .map_err(|_| StarryError::InvalidInput)?;
                let refined = expose_hw_info(refined);
                UserPtr::<u8>::from(arg).write_slice(current, refined.as_bytes())?;
            }
            SNDRV_PCM_IOCTL_HW_PARAMS => {
                // SAFETY: all fields are integer wire types and every bit
                // pattern is valid for the copied structure.
                let params = unsafe { UserPtr::<PcmHwParams>::from(arg).read_abi(current)? };
                let refined = self.configure(current, params)?;
                UserPtr::<u8>::from(arg).write_slice(current, refined.as_bytes())?;
            }
            SNDRV_PCM_IOCTL_HW_FREE => {
                self.run_control(current, ControlKind::HwFree, None, |request| {
                    Arc::clone(&self.session).submit_control(ControlCommand {
                        kind: ControlKind::HwFree,
                        request,
                        configure: None,
                    })
                })?;
            }
            SNDRV_PCM_IOCTL_SW_PARAMS => {
                // SAFETY: all fields are integer wire types and every bit
                // pattern is valid for the copied structure.
                let params = unsafe { UserPtr::<PcmSwParams>::from(arg).read_abi(current)? };
                let result = self.session.set_sw_params(params);
                let output = match &result {
                    Ok(canonical) => *canonical,
                    Err(_) => params,
                };
                // Linux copies the local parameters back even when the state
                // transition failed. A copyout fault takes precedence.
                UserPtr::<u8>::from(arg).write_slice(current, output.as_bytes())?;
                result.map(|_| ()).map_err(SoundError::into_starry)?;
            }
            SNDRV_PCM_IOCTL_STATUS | SNDRV_PCM_IOCTL_STATUS_EXT => {
                let status = self.session.status();
                UserPtr::<u8>::from(arg).write_slice(current, status.as_bytes())?;
            }
            SNDRV_PCM_IOCTL_DELAY => {
                UserPtr::<i64>::from(arg).write(current, self.session.delay())?;
            }
            SNDRV_PCM_IOCTL_HWSYNC => {}
            SNDRV_PCM_IOCTL_SYNC_PTR => {
                // SAFETY: all fields are integer wire types and every bit
                // pattern is valid for the copied structure.
                let request = unsafe { UserPtr::<PcmSyncPtr>::from(arg).read_abi(current)? };
                let request = self
                    .session
                    .update_sync_ptr(request)
                    .map_err(SoundError::into_starry)?;
                UserPtr::<u8>::from(arg).write_slice(current, request.as_bytes())?;
            }
            SNDRV_PCM_IOCTL_PREPARE => {
                self.run_control(current, ControlKind::Prepare, None, |request| {
                    Arc::clone(&self.session).submit_control(ControlCommand {
                        kind: ControlKind::Prepare,
                        request,
                        configure: None,
                    })
                })?;
            }
            SNDRV_PCM_IOCTL_START => {
                self.run_control(current, ControlKind::Start, None, |request| {
                    Arc::clone(&self.session).submit_control(ControlCommand {
                        kind: ControlKind::Start,
                        request,
                        configure: None,
                    })
                })?;
            }
            SNDRV_PCM_IOCTL_DROP => {
                self.run_control(current, ControlKind::Drop, None, |request| {
                    Arc::clone(&self.session).submit_control(ControlCommand {
                        kind: ControlKind::Drop,
                        request,
                        configure: None,
                    })
                })?;
            }
            SNDRV_PCM_IOCTL_DRAIN => {
                self.run_drain(current, |request| {
                    Arc::clone(&self.session).submit_control(ControlCommand {
                        kind: ControlKind::Drain,
                        request,
                        configure: None,
                    })
                })?;
            }
            SNDRV_PCM_IOCTL_XRUN => self.session.mark_xrun(),
            SNDRV_PCM_IOCTL_WRITEI_FRAMES => {
                // SAFETY: all fields are integer wire types and every bit
                // pattern is valid for the copied structure.
                let request = unsafe { UserPtr::<PcmXferi>::from(arg).read_abi(current)? };
                let result = self.writei(current, request)?;
                UserPtr::<u8>::from(arg).write_slice(current, result.as_bytes())?;
            }
            SNDRV_PCM_IOCTL_READI_FRAMES => {
                // SAFETY: all fields are integer wire types and every bit
                // pattern is valid for the copied structure.
                let request = unsafe { UserPtr::<PcmXferi>::from(arg).read_abi(current)? };
                let result = self.readi(current, request)?;
                UserPtr::<u8>::from(arg).write_slice(current, result.as_bytes())?;
            }
            _ => return Err(StarryError::NotATty),
        }
        Ok(0)
    }

    fn file_mmap(&self) -> StarryResult<(ax_fs_ng::vfs::FileBackend, ax_fs_ng::vfs::FileFlags)> {
        Err(StarryError::NoSuchDevice)
    }

    fn device_mmap(&self, offset: u64, _length: u64) -> StarryResult<DeviceMmap> {
        let phase = self.session.phase();
        if phase == StreamPhase::Failed {
            return Err(StarryError::Errno(Errno::EBADFD));
        }

        // Keep the reserved status/control offsets on the existing generic
        // fallback path so ALSA continues to use SYNC_PTR.
        if matches!(
            offset,
            SNDRV_PCM_MMAP_OFFSET_STATUS_OLD
                | SNDRV_PCM_MMAP_OFFSET_CONTROL_OLD
                | SNDRV_PCM_MMAP_OFFSET_STATUS_NEW
                | SNDRV_PCM_MMAP_OFFSET_CONTROL_NEW
        ) {
            return Ok(DeviceMmap::None);
        }

        // This runtime does not advertise SNDRV_PCM_INFO_MMAP. Linux reports
        // EBADFD for OPEN and ENXIO once a runtime is configured.
        match phase {
            StreamPhase::Open => Err(StarryError::Errno(Errno::EBADFD)),
            _ => Err(StarryError::NoSuchDeviceOrAddress),
        }
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

impl Pollable for PcmFile {
    fn poll(&self) -> IoEvents {
        self.session.poll_events()
    }

    unsafe fn register_shared(
        &self,
        sink: &mut dyn axpoll::SharedRegistrationSink,
        events: IoEvents,
    ) {
        let interests = events & (IoEvents::IN | IoEvents::OUT | IoEvents::ERR);
        if !interests.is_empty() {
            // SAFETY: readiness polling runs in task context and the session
            // publishes state before waking its PollSet.
            unsafe { sink.register_shared(self.session.poll_set(), interests) };
        }
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        let interests = events & (IoEvents::IN | IoEvents::OUT | IoEvents::ERR);
        if !interests.is_empty() {
            // SAFETY: same registration contract as `register_shared`.
            unsafe { sink.register_exclusive(self.session.poll_set(), interests) };
        }
    }
}

fn expose_hw_info(mut params: PcmHwParams) -> PcmHwParams {
    params.info = config::hw_info_flags();
    params.msbits = 16;
    params.rate_num = 48_000;
    params.rate_den = 1;
    params.fifo_size = params
        .intervals
        .get(buffer_size_interval_index())
        .and_then(|interval| interval.exact())
        .map(u64::from)
        .unwrap_or(0);
    params
}

const fn buffer_size_interval_index() -> usize {
    (alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_BUFFER_SIZE - alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_SAMPLE_BITS)
        as usize
}
