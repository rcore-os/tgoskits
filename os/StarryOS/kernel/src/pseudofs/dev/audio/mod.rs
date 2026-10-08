//! One ALSA capture open-file description; dup/fork share it until last close.

use alloc::{borrow::Cow, sync::Arc};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
};

use alsa_pcm_uapi::{Info, SwParams, Timespec};
use ax_driver::audio::{Capture, Config, Error};
use ax_lazyinit::OnceLock;
use ax_std::os::arceos::task::{
    self as scheduler,
    sync::{
        WaitQueue,
        irq::{IrqWaitCell, IrqWaitRegistration},
    },
};
use axfs_ng_vfs::{DeviceId, NodeFlags, NodeType, VfsError, VfsResult};
use axpoll::{ExclusiveRegistrationSink, IoEvents, Pollable, SharedRegistrationSink};
use axpoll_set::PollSet;
use bytemuck::Zeroable;
use linux_raw_sys::general::{O_ACCMODE, O_NONBLOCK, O_WRONLY};

use crate::{
    StarryError, StarryResult,
    file::{File as KernelFile, FileLike, IoDst, Kstat},
    pseudofs::{Device, DeviceOps, DirMapping, SimpleFs},
    sync::Mutex,
    task::{
        UserTaskRef,
        future::{UserWaitOutcome, block_on_user, poll_exclusive},
    },
};

mod control;
mod params;
mod pcm;

static CARD: OnceLock<Option<Arc<Card>>> = OnceLock::new();

struct Card {
    inner: Mutex<Stream>,
    busy: AtomicBool,
    waiters: PollSet,
    readers: PollSet,
    irq_notify: Arc<IrqWaitCell>,
    service_park: WaitQueue,
    _irqs: [super::IrqRegistration; 2],
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum State {
    Open     = 0,
    Setup    = 1,
    Prepared = 2,
    Running  = 3,
    Xrun     = 4,
    Draining = 5,
}

struct Stream {
    capture: Capture,
    state: State,
    // ALSA keeps the committed geometry even if hardware faults quarantine DMA.
    config: Option<Config>,
    sw: SwParams,
    produced: u64,
    consumed: u64,
    origin: u64,
    avail_max: u64,
    trigger: Timespec,
}

impl Stream {
    fn update(&mut self) {
        if self.state != State::Running {
            return;
        }
        let config = self.config.expect("running stream has hardware parameters");
        match self.capture.progress(axklib::time::monotonic_nanos()) {
            Ok(produced) => {
                self.produced = produced.saturating_sub(self.origin);
                let avail = self.available();
                self.avail_max = self.avail_max.max(avail);
                // Reserve the DMA block currently being written. Never expose
                // partially overwritten frames, even if stop_threshold is huge.
                if avail <= u64::from(config.buffer_frames - Config::DMA_BLOCK_FRAMES)
                    && avail < self.sw.stop_threshold
                {
                    return;
                }
            }
            Err(error) => warn!("SG2002 capture: {error}"),
        }
        let _ = self.capture.stop();
        self.state = State::Xrun;
    }

    fn available(&self) -> u64 {
        self.produced.saturating_sub(self.consumed)
    }

    fn start(&mut self) -> StarryResult {
        if self.state != State::Prepared {
            return Err(syscalls::Errno::EBADFD.into());
        }
        self.capture
            .start(axklib::time::monotonic_nanos())
            .map_err(map_error)?;
        self.trigger = self.timestamp();
        self.state = State::Running;
        Ok(())
    }

    fn timestamp(&self) -> Timespec {
        let time = if self.sw.tstamp_type == 0 {
            ax_runtime::hal::time::wall_time()
        } else {
            ax_runtime::hal::time::monotonic_time()
        };
        Timespec {
            seconds: time.as_secs() as i64,
            nanoseconds: i64::from(time.subsec_nanos()),
        }
    }

    fn readiness(&mut self) -> IoEvents {
        self.update();
        let readable = IoEvents::IN | IoEvents::RDNORM;
        match self.state {
            State::Open | State::Setup | State::Xrun => readable | IoEvents::ERR,
            State::Draining if self.available() == 0 => readable | IoEvents::ERR,
            State::Draining => readable,
            _ if self.available() >= self.sw.avail_min => readable,
            _ => IoEvents::empty(),
        }
    }
}

pub(crate) struct AudioNode {
    card: Arc<Card>,
    capture: bool,
}

impl AudioNode {
    pub(crate) fn open(&self, file: ax_fs_ng::File, flags: u32) -> StarryResult<Arc<dyn FileLike>> {
        if self.capture {
            if flags & O_ACCMODE == O_WRONLY {
                return Err(StarryError::InvalidInput);
            }
            self.card
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| StarryError::ResourceBusy)?;
        }
        let opened = Arc::new(AudioFile {
            base: KernelFile::new(file, flags),
            card: self.card.clone(),
            capture: self.capture,
        });
        opened.set_nonblocking(flags & O_NONBLOCK != 0)?;
        Ok(opened)
    }
}

impl DeviceOps for AudioNode {
    fn read_at(&self, _: &mut [u8], _: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }
    fn write_at(&self, _: &[u8], _: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

struct AudioFile {
    base: KernelFile,
    card: Arc<Card>,
    capture: bool,
}

impl Drop for AudioFile {
    fn drop(&mut self) {
        if self.capture {
            let mut stream = self.card.inner.lock();
            if let Err(error) = stream.capture.release() {
                error!("SG2002 audio close: {error}");
            }
            stream.state = State::Open;
            stream.config = None;
            stream.sw = SwParams::zeroed();
            stream.produced = 0;
            stream.consumed = 0;
            stream.origin = 0;
            stream.avail_max = 0;
            stream.trigger = Timespec::zeroed();
            drop(stream);
            self.card.busy.store(false, Ordering::Release);
        }
    }
}

impl AudioFile {
    fn transfer(
        &self,
        current: &UserTaskRef,
        frames: usize,
        mut copy: impl FnMut(&[i16]) -> StarryResult,
    ) -> StarryResult<usize> {
        if frames == 0 {
            return Ok(0);
        }
        let mut stream = self.card.inner.lock();
        stream.update();
        match stream.state {
            State::Xrun => return Err(StarryError::BrokenPipe),
            State::Open | State::Setup | State::Draining => {
                return Err(syscalls::Errno::EBADFD.into());
            }
            State::Prepared if frames as u64 >= stream.sw.start_threshold => stream.start()?,
            _ => (),
        }
        drop(stream);
        let mut done = 0;
        while done < frames {
            let mut read = || {
                let mut stream = self.card.inner.lock();
                stream.update();
                match stream.state {
                    State::Xrun => return Err(StarryError::BrokenPipe),
                    State::Open | State::Setup => return Err(syscalls::Errno::EBADFD.into()),
                    State::Draining => {
                        // DRAIN completes an in-flight read, but never admits a
                        // new one. Linux wait_for_avail ends capture draining.
                        stream.state = State::Setup;
                        return Ok(0);
                    }
                    _ => (),
                }
                let count = (stream.available() as usize).min(frames - done).min(512);
                if count == 0 {
                    return Err(StarryError::WouldBlock);
                }
                let mut samples = [0i16; 512];
                stream
                    .capture
                    .copy_samples(stream.origin + stream.consumed, &mut samples[..count])
                    .map_err(map_error)?;
                stream.update();
                if stream.state == State::Xrun {
                    return Err(StarryError::BrokenPipe);
                }
                // Sleep mutex, not an IRQ/spin lock: this user copy may fault.
                copy(&samples[..count])?;
                stream.consumed += count as u64;
                Ok(count)
            };
            let attempt = poll_exclusive(
                || match read() {
                    Err(error) if error.is_would_block() && !self.nonblocking() => Poll::Pending,
                    result => Poll::Ready(result),
                },
                |registrar| {
                    // SAFETY: task context; poll_exclusive rechecks the read
                    // after registration. User poll has a separate avail_min.
                    unsafe {
                        registrar
                            .register_exclusive(&self.card.readers, IoEvents::IN | IoEvents::ERR);
                    }
                },
            );
            let result = match block_on_user(current, attempt) {
                UserWaitOutcome::Ready(result) => result,
                UserWaitOutcome::Interrupted => Err(StarryError::Interrupted),
                UserWaitOutcome::TimedOut => Err(StarryError::TimedOut),
            };
            // Transfer remaining readiness even after EFAULT, interruption or
            // draining completion; stopped DMA cannot provide another IRQ.
            self.card.wake_waiters();
            match result {
                Ok(0) => break,
                Ok(count) => done += count,
                Err(_) if done != 0 => break,
                Err(error) => return Err(error),
            }
        }
        Ok(done)
    }
}

impl FileLike for AudioFile {
    fn validate_write_access(&self) -> StarryResult {
        Err(StarryError::BadFileDescriptor)
    }
    fn stat(&self) -> StarryResult<Kstat> {
        self.base.stat()
    }
    fn path(&self) -> Cow<'_, str> {
        self.base.path()
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
    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        if !self.capture {
            return Err(StarryError::OperationNotSupported);
        }
        if self.card.inner.lock().state == State::Open {
            return Err(syscalls::Errno::EBADFD.into());
        }
        if !dst.remaining_mut().is_multiple_of(2) {
            return Err(StarryError::InvalidInput);
        }
        let current = crate::task::current_user_task();
        let frames = dst.remaining_mut() / 2;
        self.transfer(&current, frames, |samples| {
            dst.write_all(bytemuck::cast_slice(samples))?;
            Ok(())
        })
        .map(|frames| frames * 2)
    }
    fn read_vectored(&self, dst: &mut IoDst) -> StarryResult<usize> {
        if dst.remaining_mut() == 0 {
            return Ok(0);
        }
        if !self.capture {
            return Err(StarryError::OperationNotSupported);
        }
        // Linux snd_pcm_readv requires RW_NONINTERLEAVED, which is not exposed.
        Err(if self.card.inner.lock().state == State::Open {
            syscalls::Errno::EBADFD.into()
        } else {
            StarryError::InvalidInput
        })
    }
    fn ioctl(&self, current: &UserTaskRef, cmd: u32, arg: usize) -> StarryResult<usize> {
        if self.capture {
            self.pcm_ioctl(current, cmd, arg)
        } else {
            control::ioctl(&self.card, current, cmd, arg)
        }
    }
}

impl Pollable for AudioFile {
    fn poll(&self) -> IoEvents {
        if self.capture {
            self.card.inner.lock().readiness()
        } else {
            IoEvents::empty()
        }
    }
    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        // SAFETY: delegated registration preserves the caller's task context.
        unsafe { sink.register_shared(&self.card.waiters, events) };
    }
    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        // SAFETY: delegated registration preserves the caller's task context.
        unsafe { sink.register_exclusive(&self.card.waiters, events) };
    }
}

pub(super) fn devices(fs: Arc<SimpleFs>) -> Option<DirMapping> {
    let card = CARD
        .call_once(|| match Card::new() {
            Ok(card) => Some(card),
            Err(error) => {
                warn!("SG2002 audio unavailable: {error}");
                None
            }
        })
        .as_ref()?
        .clone();
    let mut directory = DirMapping::new();
    for (name, minor, capture) in [("controlC0", 0, false), ("pcmC0D0c", 24, true)] {
        directory.add(
            name,
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                DeviceId::new(116, minor),
                Arc::new(AudioNode {
                    card: card.clone(),
                    capture,
                }),
            ),
        );
    }
    Some(directory)
}

impl Card {
    fn new() -> StarryResult<Arc<Self>> {
        let ports = ax_driver::audio::take().ok_or(StarryError::NoSuchDevice)?;
        let notify = Arc::new(IrqWaitCell::new());
        let interrupt = Arc::new(ports.interrupt);
        let register = |source| -> StarryResult<_> {
            let irq = ax_runtime::irq::resolve_binding_irq(source)
                .map_err(|_| StarryError::NoSuchDevice)?;
            let notify = notify.clone();
            let interrupt = interrupt.clone();
            super::request_shared_disabled(irq, move |_| {
                if interrupt.acknowledge() {
                    let _ = notify.notify();
                    ax_runtime::hal::irq::IrqReturn::Wake
                } else {
                    ax_runtime::hal::irq::IrqReturn::Unhandled
                }
            })
            .map_err(|_| StarryError::ResourceBusy)
        };
        let [dma, i2s] = ports.irqs;
        let irqs = [register(dma)?, register(i2s)?];
        for irq in &irqs {
            irq.enable().map_err(|_| StarryError::Io)?;
        }
        let card = Arc::new(Self {
            inner: Mutex::new(Stream {
                capture: ports.capture,
                state: State::Open,
                config: None,
                sw: SwParams::zeroed(),
                produced: 0,
                consumed: 0,
                origin: 0,
                avail_max: 0,
                trigger: Timespec::zeroed(),
            }),
            busy: AtomicBool::new(false),
            waiters: PollSet::new(),
            readers: PollSet::new(),
            irq_notify: notify,
            service_park: WaitQueue::new(),
            _irqs: irqs,
        });
        let service = card.clone();
        crate::task::kernel_thread_builder("audio-irq-service".into())
            .spawn(move || service.service())
            .map_err(|_| StarryError::NoMemory)?;
        Ok(card)
    }

    fn change_stream<T>(
        &self,
        change: impl FnOnce(&mut Stream) -> StarryResult<T>,
    ) -> StarryResult<T> {
        let result = {
            let mut stream = self.inner.lock();
            change(&mut stream)
        };
        // Failed operations can also publish a terminal state. Notify after
        // releasing the mutex; stopped DMA cannot supply a later interrupt.
        self.wake_waiters();
        result
    }

    fn wake_waiters(&self) {
        let mut stream = self.inner.lock();
        let events = stream.readiness();
        let read_events = if stream.available() != 0 || stream.state == State::Draining {
            events | IoEvents::IN
        } else {
            events
        };
        drop(stream);
        // SAFETY: state is published under the sleep mutex; callbacks run in
        // task context without it. A stopped/faulted stream releases all readers.
        unsafe {
            self.waiters.wake(events);
            if read_events.contains(IoEvents::ERR) {
                self.readers.wake_all(read_events);
            } else {
                self.readers.wake(read_events);
            }
        }
    }

    fn service(&self) {
        let current = scheduler::thread::current::current_thread_handle()
            .expect("audio service is a kernel thread");
        let waiter = IrqWaitRegistration::new(current.wake_handle());
        loop {
            super::irq_service::complete_irq_service_cycle(
                self.irq_notify.register(&waiter),
                |token| self.service_park.wait_until(|| !token.is_attached()),
                || self.wake_waiters(),
            )
            .expect("single audio service waiter can quiesce");
        }
    }
}

fn map_error(error: Error) -> StarryError {
    match error {
        Error::Invalid => StarryError::InvalidInput,
        Error::Busy => StarryError::ResourceBusy,
        Error::Clock => StarryError::OperationNotSupported,
        Error::Timeout => StarryError::TimedOut,
        Error::Allocation(_) => StarryError::NoMemory,
        Error::Overrun | Error::Hardware { .. } => StarryError::BrokenPipe,
    }
}

fn pcm_info(available: bool) -> Info {
    let mut info = Info::zeroed();
    info.stream = 1;
    info.subdevices_count = 1;
    info.subdevices_avail = u32::from(available);
    copy_name(&mut info.id, b"SG2002ADC");
    copy_name(&mut info.name, b"SG2002 onboard microphone");
    info
}

fn copy_name(output: &mut [u8], name: &[u8]) {
    let len = name.len().min(output.len().saturating_sub(1));
    output[..len].copy_from_slice(&name[..len]);
}
