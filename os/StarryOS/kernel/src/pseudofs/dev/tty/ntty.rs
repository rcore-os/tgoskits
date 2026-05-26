use alloc::sync::Arc;

use axpoll::PollSet;
use spin::Lazy;

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
        ax_runtime::hal::console::read_bytes(buf)
    }
}
impl TtyWrite for Console {
    fn write(&self, buf: &[u8]) {
        ax_runtime::hal::console::write_bytes(buf);
    }
}

/// The default TTY device.
pub static N_TTY: Lazy<Arc<NTtyDriver>> = Lazy::new(new_n_tty);
static CONSOLE_INPUT_SOURCE: Lazy<Arc<PollSet>> = Lazy::new(|| Arc::new(PollSet::new()));

fn handle_console_input_irq(_irq_num: usize) {
    let events = ax_runtime::hal::console::handle_irq();
    if events.intersects(
        ax_runtime::hal::console::ConsoleIrqEvent::RX_READY
            | ax_runtime::hal::console::ConsoleIrqEvent::RX_ERROR
            | ax_runtime::hal::console::ConsoleIrqEvent::OVERRUN,
    ) {
        CONSOLE_INPUT_SOURCE.wake();
    }
}

fn new_n_tty() -> Arc<NTtyDriver> {
    // Synchronously query the connected terminal's dimensions before the
    // poll-reader task is spawned so TIOCGWINSZ reports the real host
    // terminal size.  TUI applications (e.g. ratatui-based clients) use the
    // size both to lay out panels and to map mouse-event coordinates;
    // returning a stale fallback misplaces the UI and leaves clicks/scroll
    // outside the rendered widgets.  Falls back silently to the default
    // 24x80 if the host terminal does not support CPR.
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

/// Probe the connected terminal for its current size using the
/// standard cursor-position-report sequence.
///
/// Sequence: save cursor (DECSC) -> move to (9999, 9999) -> request
/// cursor position (CPR) -> restore cursor (DECRC).  The terminal
/// clamps the move to its actual bottom-right corner before reporting
/// back, so the reply `\x1b[rows;colsR` reflects the real geometry.
/// Spin-waits up to roughly 100 ms for the reply and returns `None`
/// on timeout or parse failure.
///
/// Called once during NTTY initialisation, before the polling reader
/// task is spawned, so there is no concurrent consumer racing on the
/// UART receive FIFO.
fn query_console_size() -> Option<(u16, u16)> {
    ax_runtime::hal::console::write_bytes(b"\x1b7\x1b[9999;9999H\x1b[6n\x1b8");

    let mut buf = [0u8; 32];
    let mut len = 0usize;

    // Spin up to ~100 ms (in wall time, polled via ax_runtime::hal::time::wall_time)
    // for the `R` terminator.  Hosts that ignore CPR (jcode running under
    // a non-interactive serial, automated CI runners) will time out and
    // we fall back to the 24x80 default without blocking boot further.
    let deadline = ax_runtime::hal::time::wall_time() + core::time::Duration::from_millis(100);
    'collect: while ax_runtime::hal::time::wall_time() < deadline {
        let mut tmp = [0u8; 1];
        if ax_runtime::hal::console::read_bytes(&mut tmp) > 0 {
            if len < buf.len() {
                buf[len] = tmp[0];
                len += 1;
            } else {
                // Buffer full without seeing 'R'; give up rather than
                // spinning until the deadline on a misbehaving terminal.
                break 'collect;
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
    let irq = ax_runtime::hal::console::irq_num()?;
    if !ax_runtime::hal::irq::register(irq, handle_console_input_irq) {
        warn!("Failed to register console IRQ handler for irq {irq}, falling back to polling mode");
        return None;
    }

    ax_runtime::hal::console::set_input_irq_enabled(true);
    Some(ProcessMode::InterruptDriven(CONSOLE_INPUT_SOURCE.clone()))
}
