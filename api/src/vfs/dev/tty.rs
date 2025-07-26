use alloc::sync::Arc;
use core::{
    any::Any,
    ops::{Deref, DerefMut},
};

use axerrno::{LinuxError, LinuxResult};
use axsync::Mutex;
use axtask::current;
use bytemuck::AnyBitPattern;
use lazy_static::lazy_static;
use starry_core::{
    task::AsThread,
    terminal::{
        job::JobControl,
        ldisc::LineDiscipline,
        termios::{Termios, Termios2},
    },
};
use starry_process::Process;
use starry_vm::{VmMutPtr, VmPtr};

use crate::vfs::DeviceOps;

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
            TCSETS => {
                *self.ldisc.lock().termios.deref_mut() = (arg as *const Termios).vm_read()?;
            }
            TCSETS2 => {
                self.ldisc.lock().termios = (arg as *const Termios2).vm_read()?;
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

    /// Casts the device operations to a dynamic type.
    fn as_any(&self) -> &dyn Any {
        self
    }
}
