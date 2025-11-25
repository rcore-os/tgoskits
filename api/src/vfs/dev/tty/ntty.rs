use alloc::{boxed::Box, sync::Arc};

use axhal::irq::register_irq_waker;
use lazy_static::lazy_static;

use super::Tty;
use crate::terminal::ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite};

pub type NTtyDriver = Tty<Console, Console>;

#[derive(Clone, Copy)]
pub struct Console;
impl TtyRead for Console {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        axhal::console::read_bytes(buf)
    }
}
impl TtyWrite for Console {
    fn write(&self, buf: &[u8]) {
        axhal::console::write_bytes(buf);
    }
}

lazy_static! {
    /// The default TTY device.
    pub static ref N_TTY: Arc<NTtyDriver> = new_n_tty();
}

fn new_n_tty() -> Arc<NTtyDriver> {
    Tty::new(
        Arc::default(),
        TtyConfig {
            reader: Console,
            writer: Console,
            process_mode: if let Some(irq) = axhal::console::irq_num() {
                ProcessMode::External(Box::new(move |waker| register_irq_waker(irq, &waker)) as _)
            } else {
                ProcessMode::Manual
            },
        },
    )
}
