//! Terminal module.

use alloc::sync::Arc;
use core::sync::atomic::AtomicU32;

use bytemuck::AnyBitPattern;
use kspin::SpinNoPreempt;

pub mod job;
pub mod ldisc;
pub mod termios;

#[repr(C)]
#[derive(Debug, Copy, Clone, AnyBitPattern)]
pub struct WindowSize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

pub struct Terminal {
    pub job_control: job::JobControl,
    pub window_size: SpinNoPreempt<WindowSize>,
    pub termios: SpinNoPreempt<Arc<termios::Termios2>>,
    pub pty_number: AtomicU32,
}
impl Default for Terminal {
    fn default() -> Self {
        Self {
            job_control: job::JobControl::new(),
            window_size: SpinNoPreempt::new(WindowSize {
                ws_row: 28,
                ws_col: 110,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            termios: SpinNoPreempt::new(Arc::new(termios::Termios2::default())),
            pty_number: AtomicU32::new(0),
        }
    }
}
impl Terminal {
    pub fn load_termios(&self) -> Arc<termios::Termios2> {
        self.termios.lock().clone()
    }
}
