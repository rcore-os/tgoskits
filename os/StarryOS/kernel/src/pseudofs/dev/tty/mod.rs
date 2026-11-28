mod ptm;
mod pts;
mod pty;
mod serial;
mod terminal;
mod usb_serial;

use alloc::{
    format,
    string::String,
    sync::{Arc, Weak},
};
use core::{
    any::Any,
    ops::Deref,
    sync::atomic::{AtomicUsize, Ordering},
};

use axfs_ng_vfs::{DeviceId, Location, NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, Pollable};
use starry_signal::{SignalInfo, Signo};

pub(crate) use self::pts::{DevPtsOptions, PtsInstance};
use self::terminal::{
    Terminal, WindowSize,
    ldisc::{LineDiscipline, ProcessMode, TtyConfig, TtyRead, TtyWrite, write_output_bytes},
    termios::{Termios, Termios2},
};
pub use self::{
    ptm::Ptmx,
    pts::PtsDir,
    pty::PtyDriver,
    serial::{arm_console_irq, console_device, serial_tty_entries},
    usb_serial::usb_serial_tty,
};
use crate::{
    StarryError, StarryResult,
    mm::{VmMutPtr, VmPtr},
    pseudofs::{Device, DeviceOps},
    sync::{Mutex, RawSpinLock},
    task::{
        PgidNumber, PidView, Process, current_user_task, send_signal_to_process_group,
        send_signal_to_process_group_ref,
    },
};

const TCIFLUSH: usize = 0;
const TCOFLUSH: usize = 1;
const TCIOFLUSH: usize = 2;

enum TermiosUpdate {
    Immediate,
    Drain,
    DrainAndFlush,
}

impl TermiosUpdate {
    fn from_ioctl(cmd: u32) -> Self {
        use linux_raw_sys::ioctl::*;
        match cmd {
            TCSETSF | TCSETSF2 => Self::DrainAndFlush,
            TCSETSW | TCSETSW2 => Self::Drain,
            _ => Self::Immediate,
        }
    }
}

pub(crate) enum TerminalDevice {
    Location(Location),
    Path(String),
}

struct BoundTty<R, W> {
    tty: Arc<Tty<R, W>>,
    location: Option<Location>,
}

pub(crate) fn terminal_device(term: &(dyn Any + Send + Sync)) -> Option<TerminalDevice> {
    if let Some(bound) = term.downcast_ref::<BoundTty<pty::PtyReader, pty::PtyWriter>>() {
        bound.location.clone().map_or_else(
            || {
                Some(TerminalDevice::Path(format!(
                    "/dev/pts/{}",
                    bound.tty.pty_number()
                )))
            },
            |location| Some(TerminalDevice::Location(location)),
        )
    } else if let Some(bound) =
        term.downcast_ref::<BoundTty<usb_serial::UsbSerialReader, usb_serial::UsbSerialWriter>>()
    {
        Some(TerminalDevice::Path(format!(
            "/dev/ttyUSB{}",
            bound.tty.usb_serial_number()
        )))
    } else {
        term.downcast_ref::<BoundTty<serial::SerialReader, serial::SerialWriter>>()
            .map(|bound| TerminalDevice::Path(format!("/dev/ttyS{}", bound.tty.serial_number())))
    }
}

/// Tty device
pub struct Tty<R, W> {
    this: Weak<Self>,
    terminal: Arc<Terminal>,
    ldisc: Mutex<LineDiscipline<R, W>>,
    writer: W,
    is_ptm: bool,
    termios_target: Option<Arc<Self>>,
    open_count: AtomicUsize,
    binding: RawSpinLock<Option<Weak<dyn Any + Send + Sync>>>,
}

impl<R: TtyRead, W: TtyWrite + Clone> Tty<R, W> {
    fn new(terminal: Arc<Terminal>, config: TtyConfig<R, W>) -> Arc<Self> {
        Self::new_with_termios_target(terminal, config, None)
    }

    fn new_with_termios_target(
        terminal: Arc<Terminal>,
        config: TtyConfig<R, W>,
        termios_target: Option<Arc<Self>>,
    ) -> Arc<Self> {
        let writer = config.writer.clone();
        let is_ptm = matches!(&config.process_mode, ProcessMode::Passive(_));
        let ldisc = Mutex::new(LineDiscipline::new(terminal.clone(), config));
        Arc::new_cyclic(|this| Self {
            this: this.clone(),
            terminal,
            ldisc,
            writer,
            is_ptm,
            termios_target,
            open_count: AtomicUsize::new(0),
            binding: RawSpinLock::new(None),
        })
    }
}

impl<R: TtyRead, W: TtyWrite> Tty<R, W> {
    fn termios_update_lock(&self) -> &Mutex<()> {
        &self.terminal.termios_update
    }

    fn set_termios(&self, termios: Termios2, update: TermiosUpdate) -> StarryResult<()> {
        // Linux applies a PTY master's termios requests to the slave.
        let tty = self.termios_target.as_deref().unwrap_or(self);
        let old = tty.terminal.load_termios();
        let leaving_canonical_mode = old.canonical() && !termios.canonical();
        tty.writer.update_termios(
            &old,
            &termios,
            !matches!(update, TermiosUpdate::Immediate),
            &mut || {
                if matches!(update, TermiosUpdate::DrainAndFlush) {
                    tty.ldisc.lock().drain_input()?;
                }
                Ok(())
            },
            &mut |applied| {
                // Publish the mode and its pending input together, after the TX wait.
                let ldisc = leaving_canonical_mode.then(|| tty.ldisc.lock());
                *tty.terminal.termios.lock_irqsave() = Arc::new(*applied);
                if let Some(ldisc) = ldisc {
                    ldisc.release_canonical_input();
                }
            },
        )
    }

    fn flush_queues(&self, queues: usize) -> StarryResult<()> {
        let mut ldisc = self.ldisc.lock();
        match queues {
            TCIFLUSH => return ldisc.drain_input(),
            TCIOFLUSH => ldisc.drain_input()?,
            TCOFLUSH => {}
            _ => return Err(StarryError::InvalidInput),
        }
        ldisc.discard_echo();
        // A termios update may already hold the output lock during input flush.
        drop(ldisc);
        self.writer.discard_output()
    }

    pub fn bind_to(self: &Arc<Self>, proc: &Process) -> StarryResult<()> {
        self.bind_to_at(proc, None)
    }

    fn bind_to_at(
        self: &Arc<Self>,
        proc: &Process,
        location: Option<Location>,
    ) -> StarryResult<()> {
        let pg = proc.group();
        if pg.session().sid().pid_number() != proc.pid().pid_number() {
            return Err(StarryError::OperationNotPermitted);
        }
        let binding: Arc<dyn Any + Send + Sync> = Arc::new(BoundTty {
            tty: self.clone(),
            location,
        });
        let session = pg.session();
        if !session.try_set_terminal_with(|| {
            self.terminal.job_control.set_session(&pg.session())?;
            *self.binding.lock_irqsave() = Some(Arc::downgrade(&binding));
            Ok::<_, StarryError>(binding.clone())
        })? {
            return Err(StarryError::ResourceBusy);
        }

        self.terminal.job_control.set_foreground(&pg).unwrap();
        if let Some(proc_data) = proc.identity().live_data() {
            proc_data.set_controlling_terminal(binding);
        }
        Ok(())
    }

    pub fn pty_number(&self) -> u32 {
        self.terminal.pty_number.load(Ordering::Acquire)
    }

    /// Every refusal is silent, as in Linux `tty_open_proc_set_tty()`: the
    /// caller must lead a session that has no terminal, and no other session
    /// may own this one.
    fn acquire_on_open(&self, proc: &Process, location: Option<Location>) {
        if !self.is_ptm
            && let Some(this) = self.this.upgrade()
        {
            let _ = this.bind_to_at(proc, location);
        }
    }

    fn bind_current_to_at(&self, location: Location) -> StarryResult<()> {
        self.this.upgrade().unwrap().bind_to_at(
            &current_user_task().as_thread().proc_data.proc,
            Some(location),
        )
    }
}

pub(crate) fn bind_pty_at_location(location: Location) -> Option<StarryResult<usize>> {
    let device = location.entry().downcast::<Device>().ok()?;
    let pty = device.inner().as_any().downcast_ref::<PtyDriver>()?;
    Some(pty.bind_current_to_at(location).map(|()| 0))
}

/// Linux `tty_open()` makes a tty opened for reading without `O_NOCTTY` the
/// caller's controlling terminal. Pty masters and `/dev/console` never are.
pub(crate) fn set_controlling_terminal_on_open(
    current: &crate::task::UserTaskRef,
    location: &Location,
) {
    let Ok(device) = location.entry().downcast::<Device>() else {
        return;
    };
    if location
        .metadata()
        .is_ok_and(|metadata| metadata.rdev == DeviceId::new(5, 1))
    {
        return;
    }
    let proc = &current.as_thread().proc_data.proc;
    let inner = device.inner().as_any();
    if let Some(pty) = inner.downcast_ref::<PtyDriver>() {
        pty.acquire_on_open(proc, Some(location.clone()));
    } else if let Some(tty) = inner.downcast_ref::<serial::SerialTtyDriver>() {
        tty.acquire_on_open(proc, None);
    } else if let Some(tty) = inner.downcast_ref::<usb_serial::UsbSerialTtyDriver>() {
        tty.acquire_on_open(proc, None);
    }
}

/// The PID namespace the caller sees; Linux resolves job control ids in it.
fn caller_view(current: &crate::task::UserTaskRef) -> PidView {
    PidView::new(current.as_thread().active_pid_namespace())
}

impl<R: TtyRead, W: TtyWrite> DeviceOps for Tty<R, W> {
    fn open(&self, _exclusive: bool) -> VfsResult<()> {
        // The writer accounts for the open before the count moves: a pty refuses
        // an open whose peer is already gone, and a refused open must not count.
        self.writer.opened().map_err(VfsError::from)?;
        self.open_count.fetch_add(1, Ordering::AcqRel);
        if let Err(error) = self.writer.open() {
            self.open_count.fetch_sub(1, Ordering::AcqRel);
            self.writer.closing();
            return Err(error.into());
        }
        Ok(())
    }

    fn close(&self, _exclusive: bool) {
        self.writer.closing();
        // On the last fd close, notify the writer side so the peer reader can
        // observe POLLHUP / EOF. Without this, a PTY master/slave close never
        // wakes the peer and poll()/read() hang.
        if self
            .open_count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            })
            .is_ok_and(|old| old == 1)
        {
            self.writer.close();
        }
    }

    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        if self.is_ptm || self.terminal.job_control.current_in_foreground() {
            self.ldisc.lock().read(buf).map_err(VfsError::from)
        } else {
            Err(VfsError::WouldBlock)
        }
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        if self.is_ptm {
            self.writer.write(buf);
        } else {
            let term = self.terminal.load_termios();
            write_output_bytes(&self.writer, term.as_ref(), buf);
        }
        Ok(buf.len())
    }

    fn ioctl(&self, current: &crate::task::UserTaskRef, cmd: u32, arg: usize) -> VfsResult<usize> {
        let operation = || -> StarryResult<usize> {
            use linux_raw_sys::ioctl::*;
            match cmd {
                TCGETS => {
                    let termios = *self.terminal.termios.lock_irqsave().as_ref().deref();
                    (arg as *mut Termios).vm_write(current, termios)?;
                }
                TCGETS2 => {
                    let termios = *self.terminal.termios.lock_irqsave().as_ref();
                    (arg as *mut Termios2).vm_write(current, termios)?;
                }
                TCSETS | TCSETSF | TCSETSW => {
                    // Note: vm_read() must complete before acquiring the terminal lock.
                    // Faultable user memory access inside an atomic context (preemption
                    // disabled) will call might_sleep() in handle_page_fault and panic.
                    let legacy_termios = (arg as *const Termios).vm_read(current)?;
                    let _update = self.termios_update_lock().lock();
                    // The legacy ABI has no numeric speed fields for BOTHER.
                    let mut termios = self.terminal.load_termios().with_legacy(legacy_termios);
                    termios.normalize_baudrates();
                    self.set_termios(termios, TermiosUpdate::from_ioctl(cmd))?;
                }
                TCSETS2 | TCSETSF2 | TCSETSW2 => {
                    let mut termios = (arg as *const Termios2).vm_read(current)?;
                    termios.normalize_baudrates();
                    let _update = self.termios_update_lock().lock();
                    self.set_termios(termios, TermiosUpdate::from_ioctl(cmd))?;
                }
                TIOCGPGRP => {
                    let foreground = self
                        .terminal
                        .job_control
                        .foreground()
                        .ok_or(StarryError::NoSuchProcess)?;
                    // Linux tiocgpgrp() answers with pid_vnr(): the number the
                    // caller's PID namespace shows, and 0 when that namespace
                    // cannot name the group at all.
                    let pgid = caller_view(current)
                        .visible_group_number(&foreground.identity())
                        .map_or(0, |pgid| pgid.get());
                    (arg as *mut u32).vm_write(current, pgid)?;
                }
                TIOCSPGRP => {
                    let pgid: u32 = (arg as *const u32).vm_read(current)?;
                    let pg = caller_view(current).resolve_group(PgidNumber::try_from(pgid)?)?;
                    self.terminal.job_control.set_foreground(&pg)?;
                }
                TIOCGWINSZ => {
                    let window_size = *self.terminal.window_size.lock_irqsave();
                    (arg as *mut WindowSize).vm_write(current, window_size)?;
                }
                TIOCSWINSZ => {
                    let window_size = (arg as *const WindowSize).vm_read(current)?;
                    let old = {
                        let mut guard = self.terminal.window_size.lock_irqsave();
                        let old = *guard;
                        *guard = window_size;
                        old
                    };
                    // Match Linux tty_do_resize(): notify the foreground process
                    // group via SIGWINCH so TUI applications (e.g. ratatui) can
                    // re-layout when the user resizes the host terminal.
                    let changed =
                        old.ws_row != window_size.ws_row || old.ws_col != window_size.ws_col;
                    if changed && let Some(pg) = self.terminal.job_control.foreground() {
                        let _ = send_signal_to_process_group(
                            pg.pgid_number(),
                            Some(SignalInfo::new_kernel(Signo::SIGWINCH)),
                        );
                    }
                }
                TCSBRK => {
                    self.writer.drain()?;
                    if arg == 0 {
                        return Err(StarryError::Unsupported);
                    }
                }
                TCSBRKP => {
                    self.writer.drain()?;
                    return Err(StarryError::Unsupported);
                }
                TCFLSH => self.flush_queues(arg)?,
                TIOCSPTLCK => {}
                TIOCGPTN => {
                    (arg as *mut u32).vm_write(current, self.pty_number())?;
                }
                TIOCSCTTY => {
                    self.this
                        .upgrade()
                        .unwrap()
                        .bind_to(&current.as_thread().proc_data.proc)?;
                }
                TIOCNOTTY => {
                    let proc_data = &current.as_thread().proc_data;
                    let binding = proc_data
                        .controlling_terminal()
                        .ok_or(StarryError::NotATty)?;
                    let this = self.this.upgrade().ok_or(StarryError::BadState)?;
                    let Some(bound) = binding.downcast_ref::<BoundTty<R, W>>() else {
                        return Err(StarryError::NotATty);
                    };
                    if !Arc::ptr_eq(&bound.tty, &this) {
                        return Err(StarryError::NotATty);
                    }
                    if !proc_data.clear_controlling_terminal_if(&binding) {
                        return Err(StarryError::NotATty);
                    }

                    let proc = proc_data.proc.clone();
                    let session = proc.group().session();
                    let is_session_leader = session.sid().pid_number() == proc.pid().pid_number();
                    if is_session_leader {
                        if let Some(foreground) = self.terminal.job_control.foreground() {
                            let _ = send_signal_to_process_group_ref(
                                &foreground,
                                Some(SignalInfo::new_kernel(Signo::SIGHUP)),
                            );
                            let _ = send_signal_to_process_group_ref(
                                &foreground,
                                Some(SignalInfo::new_kernel(Signo::SIGCONT)),
                            );
                        }
                        session.clear_terminal_from_processes(&binding);
                        if session.unset_terminal(&binding) {
                            *self.binding.lock_irqsave() = None;
                            self.terminal.job_control.clear_session(&session);
                        }
                    }
                }
                _ => return Err(StarryError::NotATty),
            }
            Ok(0)
        };
        operation().map_err(VfsError::from)
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }

    /// Casts the device operations to a dynamic type.
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

impl<R: TtyRead, W: TtyWrite> Pollable for Tty<R, W> {
    fn poll(&self) -> IoEvents {
        let _ = self.writer.open();
        let mut events = IoEvents::OUT | self.terminal.job_control.poll();
        if self.is_ptm || events.contains(IoEvents::IN) {
            events.set(IoEvents::IN, self.ldisc.lock().poll_read());
        }
        events
    }

    unsafe fn register_shared(
        &self,
        sink: &mut dyn axpoll::SharedRegistrationSink,
        events: IoEvents,
    ) {
        let _ = self.writer.open();
        if !self.is_ptm {
            unsafe { self.terminal.job_control.register_shared(sink, events) };
        }
        if events.contains(IoEvents::IN) {
            let source = self.ldisc.lock().rx_poll_source();
            unsafe { sink.register_shared(&source, IoEvents::IN) };
        }
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        let _ = self.writer.open();
        if !self.is_ptm {
            unsafe {
                self.terminal
                    .job_control
                    .register_shared(sink.as_shared(), events)
            };
        }
        if events.contains(IoEvents::IN) {
            let source = self.ldisc.lock().rx_poll_source();
            unsafe { sink.register_exclusive(&source, IoEvents::IN) };
        }
    }
}

pub struct CurrentTty;
impl DeviceOps for CurrentTty {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        unreachable!()
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    fn ioctl(
        &self,
        _current: &crate::task::UserTaskRef,
        _cmd: u32,
        _arg: usize,
    ) -> VfsResult<usize> {
        unreachable!()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use alloc::{sync::Arc, vec, vec::Vec};
    use std::sync::Mutex;

    use super::{
        ProcessMode, Terminal, Termios2, TermiosUpdate, Tty, TtyConfig, TtyRead, TtyWrite,
    };
    use crate::StarryResult;

    #[derive(Clone)]
    struct TermiosOrderWriter {
        terminal: Arc<Terminal>,
        events: Arc<Mutex<Vec<&'static str>>>,
        fail_configuration: bool,
    }

    impl TtyWrite for TermiosOrderWriter {
        fn write(&self, _buf: &[u8]) {}

        fn drain(&self) -> StarryResult<()> {
            self.events
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push("drain");
            Ok(())
        }

        fn termios_changed(&self, _old: &Termios2, new: &Termios2) -> StarryResult<()> {
            let event = if self.terminal.load_termios().baudrate() == new.baudrate() {
                "configure_after_publish"
            } else {
                "configure_before_publish"
            };
            self.events
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event);
            if self.fail_configuration {
                return Err(crate::StarryError::InvalidInput);
            }
            Ok(())
        }
    }

    struct IdleReader;

    impl TtyRead for IdleReader {
        fn read(&mut self, _buf: &mut [u8]) -> usize {
            0
        }

        fn discard_input(&mut self) -> StarryResult<()> {
            Ok(())
        }
    }

    impl TermiosOrderWriter {
        fn endpoint(&self) -> Arc<Tty<IdleReader, Self>> {
            Tty::new(
                self.terminal.clone(),
                TtyConfig {
                    reader: IdleReader,
                    writer: self.clone(),
                    process_mode: ProcessMode::Passive(Arc::new(axpoll_set::PollSet::new())),
                },
            )
        }
    }

    #[test]
    fn tty_endpoints_serialize_updates_to_shared_termios() {
        let terminal = Arc::new(Terminal::default());
        let writer = TermiosOrderWriter {
            terminal: terminal.clone(),
            events: Arc::new(Mutex::new(Vec::new())),
            fail_configuration: false,
        };
        let left = writer.endpoint();
        let right = writer.endpoint();

        assert!(core::ptr::eq(
            left.termios_update_lock(),
            right.termios_update_lock(),
        ));
    }

    #[test]
    fn termios_hardware_update_precedes_publication() {
        let terminal = Arc::new(Terminal::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let writer = TermiosOrderWriter {
            terminal: terminal.clone(),
            events: events.clone(),
            fail_configuration: false,
        };

        writer
            .endpoint()
            .set_termios(Termios2::default_b115200(), TermiosUpdate::Drain)
            .unwrap();

        assert_eq!(
            *events.lock().unwrap_or_else(|error| error.into_inner()),
            vec!["drain", "configure_before_publish"]
        );
        assert_eq!(terminal.load_termios().baudrate(), Some(115_200));
    }

    #[test]
    fn failed_termios_hardware_update_preserves_published_state() {
        let terminal = Arc::new(Terminal::default());
        let old_baudrate = terminal.load_termios().baudrate();
        let events = Arc::new(Mutex::new(Vec::new()));
        let writer = TermiosOrderWriter {
            terminal: terminal.clone(),
            events,
            fail_configuration: true,
        };

        assert!(
            writer
                .endpoint()
                .set_termios(Termios2::default_b115200(), TermiosUpdate::Immediate)
                .is_err()
        );
        assert_eq!(terminal.load_termios().baudrate(), old_baudrate);
    }
}

#[cfg(all(test, axtest))]
#[axtest::axtest]
fn termios_flush_preserves_input_arriving_during_uart_drain() -> axtest::AxTestResult {
    axtest::ax_assert!(
        ax_runtime::serial::input_arriving_during_uart_drain_survives_flush_for_test()
    );
    axtest::AxTestResult::Ok
}

#[cfg(all(test, axtest))]
#[axtest::axtest]
fn termios_flush_precedes_configuration() -> axtest::AxTestResult {
    use alloc::vec::Vec;

    use axtest::{ax_assert, ax_assert_eq};

    #[derive(Clone)]
    struct Output {
        events: Arc<Mutex<Vec<&'static str>>>,
        endpoint: Arc<Mutex<Weak<Tty<Input, Output>>>>,
    }

    impl TtyWrite for Output {
        fn write(&self, _buf: &[u8]) {}

        fn drain(&self) -> StarryResult<()> {
            // Probe a real input read while output is draining. Using try_lock
            // makes a regression fail deterministically instead of hanging.
            let tty = self.endpoint.lock().upgrade().unwrap();
            let mut input = tty.ldisc.try_lock().ok_or(StarryError::WouldBlock)?;
            let mut byte = [0];
            if input.read(&mut byte)? != 1 || byte != [b'A'] {
                return Err(StarryError::Io);
            }
            self.events.lock().push("drain");
            Ok(())
        }

        fn termios_changed(&self, _old: &Termios2, _new: &Termios2) -> StarryResult<()> {
            self.events.lock().push("configure");
            Ok(())
        }
    }

    struct Input {
        events: Arc<Mutex<Vec<&'static str>>>,
        fail: bool,
        byte: Option<u8>,
    }

    impl TtyRead for Input {
        fn read(&mut self, buf: &mut [u8]) -> usize {
            if let Some(byte) = self.byte.take() {
                buf[0] = byte;
                1
            } else {
                0
            }
        }

        fn discard_input(&mut self) -> StarryResult<()> {
            self.events.lock().push("flush");
            if self.fail {
                return Err(StarryError::Io);
            }
            Ok(())
        }
    }

    for fail in [false, true] {
        let terminal = Arc::new(Terminal::default());
        let old_baud = terminal.load_termios().baudrate();
        let events = Arc::new(Mutex::new(Vec::new()));
        let endpoint = Arc::new(Mutex::new(Weak::new()));
        let tty = Tty::new(
            terminal.clone(),
            TtyConfig {
                reader: Input {
                    events: events.clone(),
                    fail,
                    byte: Some(b'A'),
                },
                writer: Output {
                    events: events.clone(),
                    endpoint: endpoint.clone(),
                },
                process_mode: ProcessMode::Passive(Arc::new(axpoll_set::PollSet::new())),
            },
        );
        *endpoint.lock() = Arc::downgrade(&tty);
        let _update = tty.termios_update_lock().lock();
        let result = tty.set_termios(Termios2::default_b115200(), TermiosUpdate::DrainAndFlush);
        if fail {
            ax_assert!(matches!(result, Err(StarryError::Io)));
            ax_assert_eq!(events.lock().as_slice(), &["drain", "flush"]);
            ax_assert_eq!(terminal.load_termios().baudrate(), old_baud);
        } else {
            ax_assert!(result.is_ok());
            ax_assert_eq!(events.lock().as_slice(), &["drain", "flush", "configure"]);
            ax_assert_eq!(terminal.load_termios().baudrate(), Some(115_200));
        }
    }
    axtest::AxTestResult::Ok
}
