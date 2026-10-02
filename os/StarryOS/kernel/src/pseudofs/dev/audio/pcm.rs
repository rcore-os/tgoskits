use alsa_pcm_uapi::{
    HwParams, Info, SYNC_APPL, SYNC_AVAIL_MIN, Status, SwParams, SyncPtr, Timespec, XferI, ioctl,
};
use bytemuck::Zeroable;
use syscalls::Errno;

use super::{AudioFile, State, Stream, map_error, params, pcm_info};
use crate::{StarryError, StarryResult, file::FileLike, mm::UserPtr, task::UserTaskRef};

impl AudioFile {
    pub(super) fn pcm_ioctl(
        &self,
        current: &UserTaskRef,
        cmd: u32,
        arg: usize,
    ) -> StarryResult<usize> {
        match cmd {
            ioctl::PVERSION => UserPtr::<u32>::from(arg).write(current, 0x0002_0010)?,
            ioctl::INFO => UserPtr::<Info>::from(arg).write(current, pcm_info(false))?,
            ioctl::USER_PVERSION => {
                let _ = UserPtr::<u32>::from(arg).read(current)?;
            }
            ioctl::HW_REFINE | ioctl::HW_PARAMS => {
                let ptr = UserPtr::<HwParams>::from(arg);
                let mut params = ptr.read(current)?;
                if cmd == ioctl::HW_REFINE {
                    params::refine(&mut params)?;
                } else {
                    self.card.change_stream(|stream| stream.configure(&mut params))?;
                }
                ptr.write(current, params)?;
            }
            ioctl::HW_FREE => self.card.change_stream(|stream| {
                if !matches!(stream.state, State::Setup | State::Prepared) {
                    return Err(Errno::EBADFD.into());
                }
                let result = stream.capture.release().map_err(map_error);
                stream.config = None;
                stream.state = State::Open;
                result
            })?,
            ioctl::SW_PARAMS => {
                let ptr = UserPtr::<SwParams>::from(arg);
                let mut requested = ptr.read(current)?;
                let mut stream = self.card.inner.lock();
                let config = stream.config.ok_or(Errno::EBADFD)?;
                if !(0..=1).contains(&requested.tstamp_mode)
                    || (requested.proto >= 0x0002_000c && requested.tstamp_type > 1)
                    || requested.period_step != 1
                    || requested.avail_min == 0
                {
                    return Err(StarryError::InvalidInput);
                }
                if requested.silence_size < stream.sw.boundary {
                    if requested.silence_size > requested.silence_threshold
                        || requested.silence_threshold > u64::from(config.buffer_frames)
                    {
                        return Err(StarryError::InvalidInput);
                    }
                } else if requested.silence_threshold != 0 {
                    return Err(StarryError::InvalidInput);
                }
                requested.boundary = stream.sw.boundary;
                if requested.proto < 0x0002_000c {
                    requested.tstamp_type = stream.sw.tstamp_type;
                }
                requested.reserved.fill(0);
                requested.padding = 0;
                stream.sw = requested;
                drop(stream);
                ptr.write(current, requested)?;
            }
            ioctl::TSTAMP | ioctl::TTSTAMP => {
                let value = UserPtr::<i32>::from(arg).read(current)?;
                if !(0..=1).contains(&value) {
                    return Err(StarryError::InvalidInput);
                }
                let mut stream = self.card.inner.lock();
                if cmd == ioctl::TSTAMP {
                    stream.sw.tstamp_mode = value;
                } else {
                    stream.sw.tstamp_type = value as u32;
                }
            }
            ioctl::PREPARE => self.card.change_stream(|stream| {
                if stream.state == State::Open {
                    return Err(Errno::EBADFD.into());
                }
                if matches!(stream.state, State::Running | State::Draining) {
                    return Err(StarryError::ResourceBusy);
                }
                // prepare stops the old transfer before touching clocks/FIFO;
                // a failed reset must not leave a startable PCM state.
                stream.state = State::Setup;
                stream
                    .capture
                    .prepare(axklib::time::monotonic_nanos)
                    .map_err(map_error)?;
                stream.state = State::Prepared;
                stream.produced = 0;
                stream.consumed = 0;
                stream.origin = 0;
                stream.avail_max = 0;
                stream.trigger = Timespec::zeroed();
                Ok(())
            })?,
            ioctl::START => self.card.inner.lock().start()?,
            ioctl::DROP | ioctl::DRAIN => {
                self.card.change_stream(|stream| {
                    if stream.state == State::Open {
                        return Err(Errno::EBADFD.into());
                    }
                    stream.update();
                    // Capture DRAIN only stops a running stream. In particular,
                    // DROP and XRUN data must never become readable again.
                    if cmd == ioctl::DRAIN && stream.state != State::Running {
                        return Ok(());
                    }
                    if let Err(error) = stream.capture.stop() {
                        stream.state = State::Xrun;
                        return Err(map_error(error));
                    }
                    stream.state = if cmd == ioctl::DRAIN && stream.available() != 0 {
                        State::Draining
                    } else {
                        State::Setup
                    };
                    Ok(())
                })?;
                if cmd == ioctl::DRAIN && self.nonblocking() {
                    return Err(StarryError::WouldBlock);
                }
            }
            ioctl::RESET => {
                let mut stream = self.card.inner.lock();
                if !matches!(stream.state, State::Prepared | State::Running) {
                    return Err(Errno::EBADFD.into());
                }
                stream.origin = stream
                    .capture
                    .progress(axklib::time::monotonic_nanos())
                    .map_err(map_error)?;
                stream.produced = 0;
                stream.consumed = 0;
            }
            ioctl::STATUS | ioctl::STATUS_EXT => {
                if cmd == ioctl::STATUS_EXT {
                    let _ = UserPtr::<Status>::from(arg).read(current)?;
                }
                let mut stream = self.card.inner.lock();
                stream.update();
                let mut status = Status::zeroed();
                status.state = stream.state as i32;
                status.trigger_tstamp = stream.trigger;
                status.tstamp = stream.timestamp();
                status.hw_ptr = stream.produced % stream.sw.boundary.max(1);
                status.appl_ptr = stream.consumed % stream.sw.boundary.max(1);
                status.avail = stream.available();
                status.delay = status.avail as i64;
                status.avail_max = stream.avail_max;
                stream.avail_max = status.avail;
                drop(stream);
                UserPtr::<Status>::from(arg).write(current, status)?;
            }
            ioctl::DELAY | ioctl::HWSYNC => {
                let mut stream = self.card.inner.lock();
                if stream.config.is_none() {
                    return Err(Errno::EBADFD.into());
                }
                stream.update();
                if stream.state == State::Xrun {
                    return Err(StarryError::BrokenPipe);
                }
                let delay = stream.available() as i64;
                drop(stream);
                if cmd == ioctl::DELAY {
                    UserPtr::<i64>::from(arg).write(current, delay)?;
                }
            }
            ioctl::SYNC_PTR => {
                let ptr = UserPtr::<SyncPtr>::from(arg);
                let requested = ptr.read(current)?;
                let mut stream = self.card.inner.lock();
                if requested.flags & !7 != 0 {
                    return Err(StarryError::InvalidInput);
                }
                if requested.flags & alsa_pcm_uapi::SYNC_HWSYNC != 0 && stream.config.is_none() {
                    return Err(Errno::EBADFD.into());
                }
                stream.update();
                if requested.flags & alsa_pcm_uapi::SYNC_HWSYNC != 0 && stream.state == State::Xrun
                {
                    return Err(StarryError::BrokenPipe);
                }
                // alsa-lib initializes the SYNC_PTR fallback during open,
                // before hardware parameters establish a boundary.
                let boundary = stream.sw.boundary.max(1);
                if requested.flags & SYNC_APPL == 0 {
                    if requested.control.appl_ptr >= boundary {
                        return Err(StarryError::InvalidInput);
                    }
                    let delta = (requested.control.appl_ptr + boundary
                        - stream.consumed % boundary)
                        % boundary;
                    if delta > stream.available() {
                        return Err(StarryError::InvalidInput);
                    }
                    stream.consumed += delta;
                }
                if requested.flags & SYNC_AVAIL_MIN == 0 {
                    stream.sw.avail_min = requested.control.avail_min;
                }
                let mut response = SyncPtr::zeroed();
                response.flags = requested.flags;
                response.status.state = stream.state as i32;
                response.status.hw_ptr = stream.produced % boundary;
                response.status.tstamp = stream.timestamp();
                response.control.appl_ptr = stream.consumed % boundary;
                response.control.avail_min = stream.sw.avail_min;
                drop(stream);
                ptr.write(current, response)?;
            }
            ioctl::READI_FRAMES => {
                let ptr = UserPtr::<XferI>::from(arg);
                if self.card.inner.lock().state == State::Open {
                    return Err(Errno::EBADFD.into());
                }
                ptr.write_field(current, 0, 0i64)?;
                let mut xfer = ptr.read(current)?;
                let frames = usize::try_from(xfer.frames).map_err(|_| StarryError::InvalidInput)?;
                let bytes = frames
                    .checked_mul(2)
                    .filter(|n| *n <= isize::MAX as usize)
                    .ok_or(StarryError::InvalidInput)?;
                let mut address =
                    usize::try_from(xfer.buffer).map_err(|_| StarryError::BadAddress)?;
                address.checked_add(bytes).ok_or(StarryError::BadAddress)?;
                let result = if address == 0 {
                    Err(StarryError::InvalidInput)
                } else {
                    self.transfer(current, frames, |samples| {
                        UserPtr::<i16>::from(address).write_slice(current, samples)?;
                        address += samples.len() * 2;
                        Ok(())
                    })
                };
                xfer.result = match &result {
                    Ok(frames) => *frames as i64,
                    Err(error) => -i64::from(error.linux_errno().into_raw()),
                };
                ptr.write_field(current, 0, xfer.result)?;
                result?;
            }
            _ => return Err(StarryError::NotATty),
        }
        Ok(0)
    }
}

impl Stream {
    fn configure(&mut self, params: &mut HwParams) -> StarryResult {
        if !matches!(self.state, State::Open | State::Setup | State::Prepared) {
            return Err(Errno::EBADFD.into());
        }
        let configured = params::configure(params).and_then(|config| {
            self.capture.configure(config).map_err(map_error)?;
            Ok(config)
        });
        let config = match configured {
            Ok(config) => config,
            Err(error) => {
                // Like snd_pcm_hw_params, a failed transaction discards the old
                // setup. Capture::release quarantines DMA if stopping fails.
                if let Err(release_error) = self.capture.release() {
                    warn!("SG2002 audio parameter cleanup: {release_error}");
                }
                self.config = None;
                self.state = State::Open;
                return Err(error);
            }
        };
        self.config = Some(config);
        self.state = State::Setup;
        self.produced = 0;
        self.consumed = 0;
        self.origin = 0;
        let frames = u64::from(config.buffer_frames);
        let mut boundary = frames;
        while boundary <= (i64::MAX as u64 - frames) / 2 {
            boundary *= 2;
        }
        self.sw = SwParams {
            tstamp_type: self.sw.tstamp_type,
            period_step: 1,
            avail_min: u64::from(config.period_frames),
            start_threshold: 1,
            stop_threshold: frames,
            boundary,
            ..SwParams::zeroed()
        };
        Ok(())
    }
}
