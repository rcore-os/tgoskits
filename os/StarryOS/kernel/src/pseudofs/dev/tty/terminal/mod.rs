//! Terminal module.

use alloc::sync::Arc;
use core::sync::atomic::AtomicU32;

use bytemuck::AnyBitPattern;

use crate::{StarryError, StarryResult, sync::RawSpinLock};

pub mod job;
#[cfg(all(test, axtest))]
mod job_axtest;
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
    pub window_size: RawSpinLock<WindowSize>,
    pub termios: RawSpinLock<Arc<termios::Termios2>>,
    pub pty_number: AtomicU32,
    inherited_baudrate: Option<u32>,
}
impl Default for Terminal {
    fn default() -> Self {
        Self::new(None)
    }
}
impl Terminal {
    pub(super) fn new(inherited_baudrate: Option<u32>) -> Self {
        Self {
            job_control: job::JobControl::new(),
            window_size: RawSpinLock::new(WindowSize {
                // 24x80 is the standard VT100 fallback that applications
                // expect when TIOCGWINSZ reports a "default" terminal.
                ws_row: 24,
                ws_col: 80,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            termios: RawSpinLock::new(Arc::new(
                inherited_baudrate
                    .map(termios::Termios2::with_inherited_baudrate)
                    .unwrap_or_default(),
            )),
            pty_number: AtomicU32::new(0),
            inherited_baudrate,
        }
    }

    pub fn load_termios(&self) -> Arc<termios::Termios2> {
        self.termios.lock_irqsave().clone()
    }

    pub(super) fn prepare_termios(
        &self,
        mut termios: termios::Termios2,
        legacy: bool,
    ) -> StarryResult<Arc<termios::Termios2>> {
        if let Some(baudrate) = self.inherited_baudrate
            && !termios.retain_inherited_baudrate(baudrate, legacy)
        {
            return Err(StarryError::InvalidInput);
        }
        Ok(Arc::new(termios))
    }
}
