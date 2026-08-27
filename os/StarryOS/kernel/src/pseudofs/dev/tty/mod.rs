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
    vec::Vec,
};
use core::{
    any::Any,
    ops::Deref,
    sync::atomic::{AtomicUsize, Ordering},
    task::Context,
};

use ax_task::current;
use axfs_ng_vfs::{Location, NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, Pollable};
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};

pub(crate) use self::pts::{DevPtsMount, DevPtsOptions, PtsInstance};
use self::terminal::{
    Terminal, WindowSize,
    ldisc::{LineDiscipline, ProcessMode, TtyConfig, TtyRead, TtyWrite, write_output_bytes},
    termios::{Termios, Termios2},
};
pub use self::{
    ptm::Ptmx,
    pts::PtsDir,
    pty::PtyDriver,
    serial::{arm_console_irq, bind_console_to, console_device, serial_tty_entries},
    usb_serial::usb_serial_tty,
};
use crate::{
    StarryError, StarryResult,
    pseudofs::{Device, DeviceOps},
    sync::{IrqMutex, Mutex},
    task::{
        AsThread, PgidNumber, Process, get_process_group_by_number, send_signal_to_process_group,
    },
};

const ANSI_CURSOR_POSITION_REQUEST: &[u8] = b"\x1b[6n";
const ANSI_CURSOR_POSITION_RESPONSE: &[u8] = b"\x1b[1;1R";
const TCIFLUSH: usize = 0;
const TCOFLUSH: usize = 1;
const TCIOFLUSH: usize = 2;

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
    termios_update: Mutex<()>,
    is_ptm: bool,
    open_count: AtomicUsize,
    binding: IrqMutex<Option<Weak<dyn Any + Send + Sync>>>,
}

impl<R: TtyRead, W: TtyWrite + Clone> Tty<R, W> {
    fn new(terminal: Arc<Terminal>, config: TtyConfig<R, W>) -> Arc<Self> {
        let writer = config.writer.clone();
        let is_ptm = matches!(&config.process_mode, ProcessMode::Passive(_));
        let ldisc = Mutex::new(LineDiscipline::new(terminal.clone(), config));
        Arc::new_cyclic(|this| Self {
            this: this.clone(),
            terminal,
            ldisc,
            writer,
            termios_update: Mutex::new(()),
            is_ptm,
            open_count: AtomicUsize::new(0),
            binding: IrqMutex::new(None),
        })
    }
}

impl<R: TtyRead, W: TtyWrite> Tty<R, W> {
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
        if !pg.session().try_set_terminal_with(|| {
            self.terminal.job_control.set_session(&pg.session())?;
            let binding: Arc<dyn Any + Send + Sync> = Arc::new(BoundTty {
                tty: self.clone(),
                location,
            });
            *self.binding.lock() = Some(Arc::downgrade(&binding));
            Ok::<_, StarryError>(binding)
        })? {
            return Err(StarryError::ResourceBusy);
        }

        self.terminal.job_control.set_foreground(&pg).unwrap();
        Ok(())
    }

    pub fn pty_number(&self) -> u32 {
        self.terminal.pty_number.load(Ordering::Acquire)
    }

    fn bind_current_to_at(&self, location: Location) -> StarryResult<()> {
        self.this
            .upgrade()
            .unwrap()
            .bind_to_at(&current().as_thread().proc_data.proc, Some(location))
    }
}

pub(crate) fn bind_pty_at_location(location: Location) -> Option<StarryResult<usize>> {
    let device = location.entry().downcast::<Device>().ok()?;
    let pty = device.inner().as_any().downcast_ref::<PtyDriver>()?;
    Some(pty.bind_current_to_at(location).map(|()| 0))
}

impl<R: TtyRead, W: TtyWrite> DeviceOps for Tty<R, W> {
    fn open(&self, _exclusive: bool) -> VfsResult<()> {
        self.open_count.fetch_add(1, Ordering::AcqRel);
        self.writer.open().map_err(VfsError::from)
    }

    fn close(&self, _exclusive: bool) {
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
            let (output, response_count) = filter_cursor_position_requests(buf);
            let term = self.terminal.load_termios();
            write_output_bytes(&self.writer, term.as_ref(), &output);
            if response_count > 0 {
                let mut ldisc = self.ldisc.lock();
                for _ in 0..response_count {
                    ldisc.inject_input(ANSI_CURSOR_POSITION_RESPONSE);
                }
            }
        }
        Ok(buf.len())
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        let operation = || -> StarryResult<usize> {
            use linux_raw_sys::ioctl::*;
            match cmd {
                TCGETS => {
                    let termios = *self.terminal.termios.lock().as_ref().deref();
                    (arg as *mut Termios).vm_write(termios)?;
                }
                TCGETS2 => {
                    let termios = *self.terminal.termios.lock().as_ref();
                    (arg as *mut Termios2).vm_write(termios)?;
                }
                TCSETS | TCSETSF | TCSETSW => {
                    // Note: vm_read() must complete before acquiring the terminal lock.
                    // Faultable user memory access inside an atomic context (preemption
                    // disabled) will call might_sleep() in handle_page_fault and panic.
                    let termios = Arc::new(Termios2::new((arg as *const Termios).vm_read()?));
                    let _update = self.termios_update.lock();
                    apply_termios_update(
                        &self.writer,
                        &self.terminal,
                        termios,
                        matches!(cmd, TCSETSF | TCSETSW),
                    )?;
                    if cmd == TCSETSF {
                        self.ldisc.lock().drain_input()?;
                    }
                }
                TCSETS2 | TCSETSF2 | TCSETSW2 => {
                    let termios = Arc::new((arg as *const Termios2).vm_read()?);
                    let _update = self.termios_update.lock();
                    apply_termios_update(
                        &self.writer,
                        &self.terminal,
                        termios,
                        matches!(cmd, TCSETSF2 | TCSETSW2),
                    )?;
                    if cmd == TCSETSF2 {
                        self.ldisc.lock().drain_input()?;
                    }
                }
                TIOCGPGRP => {
                    let foreground = self
                        .terminal
                        .job_control
                        .foreground()
                        .ok_or(StarryError::NoSuchProcess)?;
                    (arg as *mut u32).vm_write(foreground.pgid().get())?;
                }
                TIOCSPGRP => {
                    let pgid: u32 = (arg as *const u32).vm_read()?;
                    let pg = get_process_group_by_number(PgidNumber::try_from(pgid)?)?;
                    self.terminal.job_control.set_foreground(&pg)?;
                }
                TIOCGWINSZ => {
                    let window_size = *self.terminal.window_size.lock();
                    (arg as *mut WindowSize).vm_write(window_size)?;
                }
                TIOCSWINSZ => {
                    let window_size = (arg as *const WindowSize).vm_read()?;
                    let old = {
                        let mut guard = self.terminal.window_size.lock();
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
                TCFLSH => match arg {
                    TCIFLUSH => self.ldisc.lock().drain_input()?,
                    TCOFLUSH => self.ldisc.lock().discard_output(&self.writer)?,
                    TCIOFLUSH => {
                        let mut ldisc = self.ldisc.lock();
                        ldisc.discard_output(&self.writer)?;
                        ldisc.drain_input()?;
                    }
                    _ => return Err(StarryError::InvalidInput),
                },
                TIOCSPTLCK => {}
                TIOCGPTN => {
                    (arg as *mut u32).vm_write(self.pty_number())?;
                }
                TIOCSCTTY => {
                    self.this
                        .upgrade()
                        .unwrap()
                        .bind_to(&current().as_thread().proc_data.proc)?;
                }
                TIOCNOTTY => {
                    let session = current().as_thread().proc_data.proc.group().session();
                    let this: Arc<dyn Any + Send + Sync> = self.this.upgrade().unwrap();
                    let binding = self
                        .binding
                        .lock()
                        .as_ref()
                        .and_then(Weak::upgrade)
                        .unwrap_or(this);
                    if current()
                        .as_thread()
                        .proc_data
                        .proc
                        .group()
                        .session()
                        .unset_terminal(&binding)
                    {
                        *self.binding.lock() = None;
                        self.terminal.job_control.clear_session(&session);
                        // TODO: If the process was session leader, send SIGHUP and
                        // SIGCONT to the foreground process group and all processes
                        // in the current session lose their
                        // controlling terminal.
                    } else {
                        warn!("Failed to unset terminal");
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

fn apply_termios_update<W: TtyWrite>(
    writer: &W,
    terminal: &Terminal,
    termios: Arc<Termios2>,
    drain: bool,
) -> StarryResult<()> {
    let old = terminal.load_termios();
    writer.update_termios(old.as_ref(), termios.as_ref(), drain, &mut || {
        *terminal.termios.lock() = termios.clone();
    })
}

fn filter_cursor_position_requests(bytes: &[u8]) -> (Vec<u8>, usize) {
    let mut output = Vec::with_capacity(bytes.len());
    let mut count = 0;
    let mut rest = bytes;

    while let Some(pos) = rest
        .windows(ANSI_CURSOR_POSITION_REQUEST.len())
        .position(|window| window == ANSI_CURSOR_POSITION_REQUEST)
    {
        output.extend_from_slice(&rest[..pos]);
        count += 1;
        rest = &rest[pos + ANSI_CURSOR_POSITION_REQUEST.len()..];
    }

    output.extend_from_slice(rest);
    (output, count)
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

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        let _ = self.writer.open();
        if !self.is_ptm {
            self.terminal.job_control.register(context, events);
        }
        if events.contains(IoEvents::IN) {
            self.ldisc.lock().register_rx_waker(context.waker());
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

    fn ioctl(&self, _cmd: u32, _arg: usize) -> VfsResult<usize> {
        unreachable!()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use alloc::{sync::Arc, vec, vec::Vec};

    use super::{
        Terminal, Termios2, TtyWrite, apply_termios_update, filter_cursor_position_requests,
    };
    use crate::{StarryResult, sync::Mutex};

    struct TermiosOrderWriter {
        terminal: Arc<Terminal>,
        events: Arc<Mutex<Vec<&'static str>>>,
        fail_configuration: bool,
    }

    impl TtyWrite for TermiosOrderWriter {
        fn write(&self, _buf: &[u8]) {}

        fn drain(&self) -> StarryResult<()> {
            self.events.lock().push("drain");
            Ok(())
        }

        fn termios_changed(&self, _old: &Termios2, new: &Termios2) -> StarryResult<()> {
            let event = if self.terminal.load_termios().baudrate() == new.baudrate() {
                "configure_after_publish"
            } else {
                "configure_before_publish"
            };
            self.events.lock().push(event);
            if self.fail_configuration {
                return Err(crate::StarryError::InvalidInput);
            }
            Ok(())
        }
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

        apply_termios_update(
            &writer,
            &terminal,
            Arc::new(Termios2::default_b115200()),
            true,
        )
        .unwrap();

        assert_eq!(*events.lock(), vec!["drain", "configure_before_publish"]);
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
            apply_termios_update(
                &writer,
                &terminal,
                Arc::new(Termios2::default_b115200()),
                false,
            )
            .is_err()
        );
        assert_eq!(terminal.load_termios().baudrate(), old_baudrate);
    }

    #[test]
    fn cursor_position_request_matcher_does_not_buffer_partial_writes() {
        assert_eq!(
            filter_cursor_position_requests(b"\x1b["),
            (b"\x1b[".to_vec(), 0)
        );
        assert_eq!(filter_cursor_position_requests(b"6"), (b"6".to_vec(), 0));
        assert_eq!(filter_cursor_position_requests(b"n"), (b"n".to_vec(), 0));
    }

    #[test]
    fn cursor_position_request_matcher_recovers_after_partial_mismatch() {
        assert_eq!(
            filter_cursor_position_requests(b"\x1bX"),
            (b"\x1bX".to_vec(), 0)
        );
        assert_eq!(filter_cursor_position_requests(b"\x1b[6n"), (Vec::new(), 1));
        assert_eq!(
            filter_cursor_position_requests(b"\x1b[6n\x1b[6n"),
            (Vec::new(), 2)
        );
    }

    #[test]
    fn cursor_position_request_filter_preserves_other_output() {
        assert_eq!(
            filter_cursor_position_requests(b"ab\x1b[6ncd"),
            (b"abcd".to_vec(), 1)
        );
    }

    #[test]
    fn cursor_position_request_filter_flushes_unmatched_prefix() {
        assert_eq!(
            filter_cursor_position_requests(b"\x1b[31mred"),
            (b"\x1b[31mred".to_vec(), 0)
        );

        assert_eq!(
            filter_cursor_position_requests(b"\x1b["),
            (b"\x1b[".to_vec(), 0)
        );
        assert_eq!(filter_cursor_position_requests(b"A"), (b"A".to_vec(), 0));
    }
}
