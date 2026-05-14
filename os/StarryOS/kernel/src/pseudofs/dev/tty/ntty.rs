use alloc::sync::Arc;

use axpoll::PollSet;
use lazy_static::lazy_static;

use super::{
    Tty,
    terminal::{
        WindowSize,
        ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite},
    },
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

lazy_static! {
    /// The default TTY device.
    pub static ref N_TTY: Arc<NTtyDriver> = new_n_tty();
    static ref CONSOLE_INPUT_SOURCE: Arc<PollSet> = Arc::new(PollSet::new());
}

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
    // Query the actual terminal dimensions before spawning the input reader,
    // so that TIOCGWINSZ reports the real host terminal size.  Applications
    // like jcode use the size to lay out panels and to map mouse coordinates
    // to screen regions; a stale or wrong size breaks mouse scroll entirely.
    let terminal = {
        let t = super::terminal::Terminal::default();
        if let Some((rows, cols)) = query_console_size() {
            *t.window_size.lock() = WindowSize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
        }
        Arc::new(t)
    };

    Tty::new(
        terminal,
        TtyConfig {
            reader: Console,
            writer: Console,
            process_mode: console_irq_mode().unwrap_or(ProcessMode::Manual),
        },
    )
}

/// Query the connected terminal's dimensions using the standard cursor-position
/// interrogation sequence (CPR / DECXCPR).
///
/// Sequence: save cursor → move to (9999,9999) → request CPR → restore cursor.
/// The terminal replies with `\x1b[rows;colsR`.  We spin-wait up to ~100 ms
/// for the reply and return `None` on timeout or parse failure.
///
/// This is called once, synchronously, before the poll-reader task is spawned,
/// so there is no race on the UART receive FIFO.
fn query_console_size() -> Option<(u16, u16)> {
    // save cursor, jump to extreme bottom-right, request CPR, restore cursor
    ax_hal::console::write_bytes(b"\x1b7\x1b[9999;9999H\x1b[6n\x1b8");

    let mut buf = [0u8; 32];
    let mut len = 0usize;

    // Spin-wait up to ~100 ms for the `R` terminator.  On a 1 GHz CPU 60 M
    // iterations ≈ 60 ms; on a 3 GHz host the round-trip is well under 1 ms.
    'collect: for _ in 0..60_000_000usize {
        let mut tmp = [0u8; 1];
        if ax_hal::console::read_bytes(&mut tmp) > 0 {
            if len < buf.len() {
                buf[len] = tmp[0];
                len += 1;
            }
            if tmp[0] == b'R' {
                break 'collect;
            }
        }
        core::hint::spin_loop();
    }

    if len == 0 {
        return None;
    }

    // The response is `\x1b[rows;colsR`.  Search for the *last* `\x1b[`
    // before the `R` to skip any garbage that may precede it.
    let r_pos = buf[..len].iter().rposition(|&b| b == b'R')?;
    let escape_pos = buf[..r_pos].windows(2).rposition(|w| w == b"\x1b[")?;
    let inner = core::str::from_utf8(&buf[escape_pos + 2..r_pos]).ok()?;
    let mut parts = inner.splitn(2, ';');
    let rows: u16 = parts.next()?.parse().ok()?;
    let cols: u16 = parts.next()?.parse().ok()?;
    if rows == 0 || cols == 0 {
        return None;
    }
    Some((rows, cols))
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
