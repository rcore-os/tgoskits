//! Terminal module.

use alloc::sync::Arc;
use core::sync::atomic::AtomicU32;

#[cfg(not(all(test, not(target_os = "none"))))]
use ax_kspin::SpinNoIrq as TerminalStateLock;
#[cfg(all(test, not(target_os = "none")))]
use ax_kspin::SpinRaw as TerminalStateLock;
use bytemuck::AnyBitPattern;

pub mod job;
pub mod ldisc;
pub mod termios;

#[repr(C)]
#[derive(Debug, Copy, Clone, AnyBitPattern, bytemuck::NoUninit)]
pub struct WindowSize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

pub struct Terminal {
    pub job_control: job::JobControl,
    pub window_size: TerminalStateLock<WindowSize>,
    pub termios: TerminalStateLock<Arc<termios::Termios2>>,
    pub pty_number: AtomicU32,
}
impl Default for Terminal {
    fn default() -> Self {
        Self {
            job_control: job::JobControl::new(),
            window_size: TerminalStateLock::new(WindowSize {
                // 24x80 is the standard VT100 fallback that applications
                // expect when TIOCGWINSZ reports a "default" terminal.
                ws_row: 24,
                ws_col: 80,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            termios: TerminalStateLock::new(Arc::new(termios::Termios2::default())),
            pty_number: AtomicU32::new(0),
        }
    }
}
impl Terminal {
    pub fn load_termios(&self) -> Arc<termios::Termios2> {
        self.termios.lock().clone()
    }
}
