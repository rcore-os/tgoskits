use alloc::sync::Arc;
use axfs_ng_vfs::NodeFlags;
use core::{
    any::Any,
    ops::{Deref, DerefMut},
    task::Context,
};

use axerrno::{LinuxError, LinuxResult};
use axio::{IoEvents, Pollable};
use axsync::Mutex;
use axtask::current;
use bytemuck::AnyBitPattern;
use lazy_static::lazy_static;
use starry_core::task::AsThread;
use starry_process::Process;
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    terminal::{
        job::JobControl,
        ldisc::LineDiscipline,
        termios::{Termios, Termios2},
    },
    vfs::DeviceOps,
};

#[repr(C)]
#[derive(Debug, Copy, Clone, AnyBitPattern)]
pub struct WindowSize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// Tty device
pub struct Tty {
    job_control: Arc<JobControl>,
    ldisc: Mutex<LineDiscipline>,
    window_size: Mutex<WindowSize>,
}

impl Tty {
    fn new() -> Self {
        let job_control = Arc::new(JobControl::new());
        let ldisc = Mutex::new(LineDiscipline::new(job_control.clone()));
        let window_size = Mutex::new(WindowSize {
            ws_row: 28,
            ws_col: 110,
            ws_xpixel: 0,
            ws_ypixel: 0,
        });
        Self {
            job_control,
            ldisc,
            window_size,
        }
    }

    pub fn bind_to(self: &Arc<Self>, proc: &Process) {
        let pg = proc.group();
        assert!(pg.session().set_terminal_with(|| {
            self.job_control.set_session(&pg.session());
            self.clone()
        }));

        self.job_control.set_foreground(&pg).unwrap();
    }
}

lazy_static! {
    /// The default TTY device.
    pub static ref N_TTY: Arc<Tty> = Arc::new(Tty::new());
}

impl DeviceOps for Tty {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> LinuxResult<usize> {
        self.job_control.wait_until_foreground();
        self.ldisc.lock().read(buf)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> LinuxResult<usize> {
        axhal::console::write_bytes(buf);
        Ok(buf.len())
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> LinuxResult<usize> {
        use linux_raw_sys::ioctl::*;
        match cmd {
            TCGETS => {
                (arg as *mut Termios).vm_write(*self.ldisc.lock().termios.deref())?;
            }
            TCGETS2 => {
                (arg as *mut Termios2).vm_write(self.ldisc.lock().termios)?;
            }
            TCSETS | TCSETSF | TCSETSW => {
                // TODO: drain output?
                let mut ldisc = self.ldisc.lock();
                *ldisc.termios.deref_mut() = (arg as *const Termios).vm_read()?;
                if cmd == TCSETSF {
                    ldisc.drain_input();
                }
            }
            TCSETS2 | TCSETSF2 | TCSETSW2 => {
                // TODO: drain output?
                let mut ldisc = self.ldisc.lock();
                ldisc.termios = (arg as *const Termios2).vm_read()?;
                if cmd == TCSETSF2 {
                    ldisc.drain_input();
                }
            }
            TIOCGPGRP => {
                let foreground = self.job_control.foreground().ok_or(LinuxError::ESRCH)?;
                (arg as *mut u32).vm_write(foreground.pgid())?;
            }
            TIOCSPGRP => {
                let curr = current();
                self.job_control
                    .set_foreground(&curr.as_thread().proc_data.proc.group())?;
            }
            TIOCGWINSZ => {
                (arg as *mut WindowSize).vm_write(*self.window_size.lock())?;
            }
            TIOCSWINSZ => {
                *self.window_size.lock() = (arg as *const WindowSize).vm_read()?;
            }
            _ => return Err(LinuxError::ENOTTY),
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

impl Pollable for Tty {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::OUT;
        events.set(IoEvents::IN, self.ldisc.lock().can_read());
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            context.waker().wake_by_ref();
        }
    }
}
