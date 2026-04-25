mod ntty;
mod ptm;
mod pts;
mod pty;
mod terminal;

use alloc::sync::{Arc, Weak};
use core::{any::Any, ops::Deref, sync::atomic::Ordering, task::Context};

use ax_errno::{AxError, AxResult};
use ax_sync::Mutex;
use ax_task::{
    current,
    future::{block_on, poll_io},
};
use axfs_ng_vfs::NodeFlags;
use axpoll::{IoEvents, Pollable};
use starry_process::Process;
use starry_vm::{VmMutPtr, VmPtr};

use self::terminal::{
    Terminal, WindowSize,
    ldisc::{LineDiscipline, ProcessMode, TtyConfig, TtyRead, TtyWrite},
    termios::{Termios, Termios2},
};
pub use self::{
    ntty::{N_TTY, NTtyDriver},
    ptm::Ptmx,
    pts::PtsDir,
    pty::PtyDriver,
};
use crate::{
    pseudofs::{DeviceOps, SimpleFs},
    task::{AsThread, get_process_group},
};

pub fn create_pty_master(fs: Arc<SimpleFs>) -> AxResult<Arc<PtyDriver>> {
    let (master, slave) = pty::create_pty_pair();
    pts::add_slave(fs, slave)?;
    Ok(master)
}

/// Tty device
pub struct Tty<R, W> {
    this: Weak<Self>,
    terminal: Arc<Terminal>,
    ldisc: Mutex<LineDiscipline<R, W>>,
    writer: W,
    is_ptm: bool,
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
            is_ptm,
        })
    }
}

impl<R: TtyRead, W: TtyWrite> Tty<R, W> {
    pub fn bind_to(self: &Arc<Self>, proc: &Process) -> AxResult<()> {
        let pg = proc.group();
        if pg.session().sid() != proc.pid() {
            return Err(AxError::OperationNotPermitted);
        }
        assert!(pg.session().set_terminal_with(|| {
            self.terminal.job_control.set_session(&pg.session());
            self.clone()
        }));

        self.terminal.job_control.set_foreground(&pg).unwrap();
        Ok(())
    }

    pub fn pty_number(&self) -> u32 {
        self.terminal.pty_number.load(Ordering::Acquire)
    }
}

impl<R: TtyRead, W: TtyWrite> DeviceOps for Tty<R, W> {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> AxResult<usize> {
        block_on(poll_io(
            &self.terminal.job_control,
            IoEvents::IN,
            false,
            || {
                if self.is_ptm || self.terminal.job_control.current_in_foreground() {
                    self.ldisc.lock().read(buf)
                } else {
                    Err(AxError::WouldBlock)
                }
            },
        ))
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> AxResult<usize> {
        self.writer.write(buf);
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
                // TODO: drain output?
                // Note: vm_read() must complete before acquiring the SpinNoPreempt lock.
                // Faultable user memory access inside an atomic context (preemption
                // disabled) will call might_sleep() in handle_page_fault and panic.
                let termios = Arc::new(Termios2::new((arg as *const Termios).vm_read()?));
                *self.terminal.termios.lock() = termios;
                if cmd == TCSETSF {
                    self.ldisc.lock().drain_input();
                }
            }
            TCSETS2 | TCSETSF2 | TCSETSW2 => {
                // TODO: drain output?
                let termios = Arc::new((arg as *const Termios2).vm_read()?);
                *self.terminal.termios.lock() = termios;
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
                *self.terminal.window_size.lock() = window_size;
            }
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
                if current()
                    .as_thread()
                    .proc_data
                    .proc
                    .group()
                    .session()
                    .unset_terminal(&(self.this.upgrade().unwrap() as _))
                {
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

impl<R: TtyRead, W: TtyWrite> Pollable for Tty<R, W> {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::OUT | self.terminal.job_control.poll();
        if self.is_ptm || events.contains(IoEvents::IN) {
            events.set(IoEvents::IN, self.ldisc.lock().poll_read());
        }
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
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
