use alloc::sync::Arc;

use axpoll::PollSet;
use spin::Lazy;

use super::{
    Tty,
    terminal::ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite},
};

pub type NTtyDriver = Tty<Console, Console>;

#[derive(Clone, Copy)]
pub struct Console;
impl TtyRead for Console {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        ax_hal::console::read_bytes(buf)
    }
}
impl TtyWrite for Console {
    fn write(&self, buf: &[u8]) {
        ax_hal::console::write_bytes(buf);
    }
}

/// The default TTY device.
pub static N_TTY: Lazy<Arc<NTtyDriver>> = Lazy::new(new_n_tty);
static CONSOLE_INPUT_SOURCE: Lazy<Arc<PollSet>> = Lazy::new(|| Arc::new(PollSet::new()));

fn handle_console_input_irq(_irq_num: usize) {
    let events = ax_hal::console::handle_irq();
    if events.intersects(
        ax_hal::console::ConsoleIrqEvent::RX_READY
            | ax_hal::console::ConsoleIrqEvent::RX_ERROR
            | ax_hal::console::ConsoleIrqEvent::OVERRUN,
    ) {
        CONSOLE_INPUT_SOURCE.wake();
    }
}

fn new_n_tty() -> Arc<NTtyDriver> {
    Tty::new(
        Arc::default(),
        TtyConfig {
            reader: Console,
            writer: Console,
            process_mode: console_irq_mode().unwrap_or(ProcessMode::Manual),
        },
    )
}

fn console_irq_mode() -> Option<ProcessMode> {
    let irq = ax_hal::console::irq_num()?;
    if !ax_hal::irq::register(irq, handle_console_input_irq) {
        warn!("Failed to register console IRQ handler for irq {irq}, falling back to polling mode");
        return None;
    }

    ax_hal::console::set_input_irq_enabled(true);
    Some(ProcessMode::InterruptDriven(CONSOLE_INPUT_SOURCE.clone()))
}
