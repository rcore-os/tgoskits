use alloc::sync::Arc;
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_task::IrqNotify;
use axpoll::{IoEvents, PollSet};
use spin::LazyLock;

use super::{
    Tty,
    terminal::ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite},
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
pub static N_TTY: LazyLock<Arc<NTtyDriver>> = LazyLock::new(new_n_tty);
static CONSOLE_INPUT_SOURCE: LazyLock<Arc<PollSet>> = LazyLock::new(|| Arc::new(PollSet::new()));
static CONSOLE_INPUT_NOTIFY: LazyLock<Arc<IrqNotify>> =
    LazyLock::new(|| Arc::new(IrqNotify::new()));
static CONSOLE_NOTIFY_WORKER: AtomicBool = AtomicBool::new(false);

fn handle_console_input_irq(_irq_num: usize) {
    let events = ax_runtime::hal::console::handle_irq();
    if events.intersects(
        ax_runtime::hal::console::ConsoleIrqEvent::RX_READY
            | ax_runtime::hal::console::ConsoleIrqEvent::RX_ERROR
            | ax_runtime::hal::console::ConsoleIrqEvent::OVERRUN,
    ) {
        CONSOLE_INPUT_NOTIFY.notify_irq();
    }
}

unsafe fn handle_console_input_raw_irq(
    ctx: ax_runtime::hal::irq::IrqContext,
    _data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    handle_console_input_irq(ctx.irq.0);
    ax_runtime::hal::irq::IrqReturn::Handled
}

fn new_n_tty() -> Arc<NTtyDriver> {
    let terminal = {
        let t = super::terminal::Terminal::default();

        // Synchronously querying the connected terminal only works when the
        // firmware/serial path can reliably supply a cursor-position response.
        // Dynamic-platform QEMU tests run under ostool pipes, so keep the
        // default 24x80 fallback there instead of stalling early Starry boot.
        #[cfg(not(feature = "plat-dyn"))]
        if let Some((rows, cols)) = query_console_size() {
            *t.window_size.lock() = super::terminal::WindowSize {
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

fn start_console_notify_worker() {
    if CONSOLE_NOTIFY_WORKER.swap(true, Ordering::AcqRel) {
        return;
    }
    ax_task::spawn_with_name(
        || loop {
            CONSOLE_INPUT_NOTIFY.wait();
            // Console RX readiness has been published by the IRQ handler.
            unsafe { CONSOLE_INPUT_SOURCE.wake(IoEvents::IN) };
        },
        "console-notify".into(),
    );
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
#[cfg(not(feature = "plat-dyn"))]
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

    parse_console_size_response(&buf[..len])
}

#[cfg(any(test, not(feature = "plat-dyn")))]
fn parse_console_size_response(buf: &[u8]) -> Option<(u16, u16)> {
    let r_pos = buf.iter().rposition(|&b| b == b'R')?;
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
    if ax_runtime::hal::irq::request_shared_irq(
        irq,
        handle_console_input_raw_irq,
        NonNull::dangling(),
    )
    .is_err()
    {
        warn!("Failed to register console IRQ handler for irq {irq}, falling back to polling mode");
        return None;
    }

    ax_runtime::hal::console::set_input_irq_enabled(true);
    start_console_notify_worker();
    Some(ProcessMode::InterruptDriven(CONSOLE_INPUT_SOURCE.clone()))
}

#[cfg(test)]
mod tests {
    use super::parse_console_size_response;

    #[test]
    fn parses_cursor_position_response() {
        assert_eq!(
            parse_console_size_response(b"\x1b7\x1b[24;80R\x1b8"),
            Some((24, 80))
        );
    }
}
