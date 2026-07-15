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

use ax_errno::{AxError, AxResult};
use ax_sync::PiMutex;
use axfs_ng_vfs::NodeFlags;
use axpoll::{IoEvents, Pollable};
use starry_process::Process;
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};

use self::terminal::{
    Terminal, WindowSize,
    ldisc::{LineDiscipline, ProcessMode, TtyConfig, TtyRead, TtyWrite, write_output_bytes},
    termios::{Termios, Termios2},
};
pub use self::{
    ptm::Ptmx,
    pts::PtsDir,
    pty::PtyDriver,
    serial::{
        SerialTtyDriver, arm_console_irq, bind_console_to, console_device, serial_tty_entries,
    },
    usb_serial::{UsbSerialTtyDriver, usb_serial_tty},
};
use crate::{
    pseudofs::DeviceOps,
    task::{current_user_task, get_process_group, send_signal_to_process_group},
};

const ANSI_CURSOR_POSITION_REQUEST: &[u8] = b"\x1b[6n";
const ANSI_CURSOR_POSITION_RESPONSE: &[u8] = b"\x1b[1;1R";

pub fn terminal_device_path(term: &(dyn Any + Send + Sync)) -> Option<String> {
    if let Some(pts) = term.downcast_ref::<PtyDriver>() {
        Some(format!("/dev/pts/{}", pts.pty_number()))
    } else if let Some(tty) = term.downcast_ref::<UsbSerialTtyDriver>() {
        Some(format!("/dev/ttyUSB{}", tty.usb_serial_number()))
    } else {
        term.downcast_ref::<SerialTtyDriver>()
            .map(|tty| format!("/dev/ttyS{}", tty.serial_number()))
    }
}

/// Tty device
pub struct Tty<R, W> {
    this: Weak<Self>,
    terminal: Arc<Terminal>,
    ldisc: PiMutex<LineDiscipline<R, W>>,
    writer: W,
    is_ptm: bool,
    open_count: AtomicUsize,
}

impl<R: TtyRead, W: TtyWrite + Clone> Tty<R, W> {
    fn new(terminal: Arc<Terminal>, config: TtyConfig<R, W>) -> Arc<Self> {
        let writer = config.writer.clone();
        let is_ptm = matches!(&config.process_mode, ProcessMode::Passive(_));
        let ldisc = PiMutex::new(LineDiscipline::new(terminal.clone(), config));
        Arc::new_cyclic(|this| Self {
            this: this.clone(),
            terminal,
            ldisc,
            writer,
            is_ptm,
            open_count: AtomicUsize::new(0),
        })
    }
}

impl<R: TtyRead, W: TtyWrite> Tty<R, W> {
    pub fn bind_to(self: &Arc<Self>, proc: &Process) -> AxResult<()> {
        let pg = proc.group();
        if pg.session().sid() != proc.pid() {
            return Err(AxError::OperationNotPermitted);
        }
        if !pg.session().try_set_terminal_with(|| {
            self.terminal.job_control.set_session(&pg.session())?;
            Ok::<_, AxError>(self.clone() as Arc<dyn Any + Send + Sync>)
        })? {
            return Err(AxError::ResourceBusy);
        }

        self.terminal.job_control.set_foreground(&pg).unwrap();
        Ok(())
    }

    pub fn pty_number(&self) -> u32 {
        self.terminal.pty_number.load(Ordering::Acquire)
    }
}

impl<R: TtyRead, W: TtyWrite> DeviceOps for Tty<R, W> {
    fn open(&self, _exclusive: bool) -> AxResult<()> {
        self.open_count.fetch_add(1, Ordering::AcqRel);
        self.writer.open()
    }

    fn close(&self, _exclusive: bool) {
        // On the last fd close, notify the writer side so the peer reader can
        // observe POLLHUP / EOF. Without this, a PTY master/slave close never
        // wakes the peer and poll()/read() hang.
        if self
            .open_count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            })
            .is_ok_and(|old| old == 1)
        {
            self.writer.close();
        }
    }

    fn read_at(&self, buf: &mut [u8], _offset: u64) -> AxResult<usize> {
        if self.is_ptm || self.terminal.job_control.current_in_foreground() {
            self.ldisc.lock().read(buf)
        } else {
            Err(AxError::WouldBlock)
        }
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> AxResult<usize> {
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

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
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
                if matches!(cmd, TCSETSF | TCSETSW) {
                    self.writer.drain()?;
                }
                let old = {
                    let mut guard = self.terminal.termios.lock();
                    let old = guard.clone();
                    *guard = termios.clone();
                    old
                };
                self.writer.termios_changed(old.as_ref(), termios.as_ref());
                if cmd == TCSETSF {
                    self.ldisc.lock().drain_input();
                }
            }
            TCSETS2 | TCSETSF2 | TCSETSW2 => {
                let termios = Arc::new((arg as *const Termios2).vm_read()?);
                if matches!(cmd, TCSETSF2 | TCSETSW2) {
                    self.writer.drain()?;
                }
                let old = {
                    let mut guard = self.terminal.termios.lock();
                    let old = guard.clone();
                    *guard = termios.clone();
                    old
                };
                self.writer.termios_changed(old.as_ref(), termios.as_ref());
                if cmd == TCSETSF2 {
                    self.ldisc.lock().drain_input();
                }
            }
            TIOCGPGRP => {
                let foreground = self
                    .terminal
                    .job_control
                    .foreground()
                    .ok_or(AxError::NoSuchProcess)?;
                (arg as *mut u32).vm_write(foreground.pgid())?;
            }
            TIOCSPGRP => {
                let pgid: u32 = (arg as *const u32).vm_read()?;
                let pg = get_process_group(pgid)?;
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
                let changed = old.ws_row != window_size.ws_row || old.ws_col != window_size.ws_col;
                if changed && let Some(pg) = self.terminal.job_control.foreground() {
                    let _ = send_signal_to_process_group(
                        pg.pgid(),
                        Some(SignalInfo::new_kernel(Signo::SIGWINCH)),
                    );
                }
            }
            TCSBRK => {
                self.writer.drain()?;
                if arg == 0 {
                    return Err(AxError::Unsupported);
                }
            }
            TCSBRKP => {
                self.writer.drain()?;
                return Err(AxError::Unsupported);
            }
            TIOCSPTLCK => {}
            TIOCGPTN => {
                (arg as *mut u32).vm_write(self.pty_number())?;
            }
            TIOCSCTTY => {
                self.this
                    .upgrade()
                    .unwrap()
                    .bind_to(&current_user_task().as_thread().proc_data.proc)?;
            }
            TIOCNOTTY => {
                let session = current_user_task()
                    .as_thread()
                    .proc_data
                    .proc
                    .group()
                    .session();
                if current_user_task()
                    .as_thread()
                    .proc_data
                    .proc
                    .group()
                    .session()
                    .unset_terminal(&(self.this.upgrade().unwrap() as _))
                {
                    self.terminal.job_control.clear_session(&session);
                    // TODO: If the process was session leader, send SIGHUP and
                    // SIGCONT to the foreground process group and all processes
                    // in the current session lose their
                    // controlling terminal.
                } else {
                    warn!("Failed to unset terminal");
                }
            }
            _ => return Err(AxError::NotATty),
        }
        Ok(0)
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
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> AxResult<usize> {
        unreachable!()
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> AxResult<usize> {
        Ok(0)
    }

    fn ioctl(&self, _cmd: u32, _arg: usize) -> AxResult<usize> {
        unreachable!()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::filter_cursor_position_requests;

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
