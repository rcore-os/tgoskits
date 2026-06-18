use alloc::{sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

use ax_task::IrqNotify;
use axpoll::{IoEvents, PollSet};
use spin::LazyLock;

use super::{
    Tty,
    terminal::ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite},
};

pub type NTtyDriver = Tty<ConsoleReader, Console>;

const CONSOLE_RX_BUFFER_CAP: usize = 4096;
const CONSOLE_RX_DRAIN_CHUNK: usize = 64;
const CONSOLE_RX_IDLE: u8 = 0;
const CONSOLE_RX_IRQ_BORROW: u8 = 1;
const CONSOLE_RX_TASK_BORROW: u8 = 2;

struct ConsoleRxRing {
    buf: [u8; CONSOLE_RX_BUFFER_CAP],
    head: usize,
    len: usize,
}

impl ConsoleRxRing {
    const fn new() -> Self {
        Self {
            buf: [0; CONSOLE_RX_BUFFER_CAP],
            head: 0,
            len: 0,
        }
    }

    fn is_full(&self) -> bool {
        self.len == CONSOLE_RX_BUFFER_CAP
    }

    fn free_len(&self) -> usize {
        CONSOLE_RX_BUFFER_CAP - self.len
    }

    fn push_slice(&mut self, bytes: &[u8]) -> usize {
        let mut written = 0;
        for &byte in bytes.iter().take(self.free_len()) {
            let tail = (self.head + self.len) % CONSOLE_RX_BUFFER_CAP;
            self.buf[tail] = byte;
            self.len += 1;
            written += 1;
        }
        written
    }

    fn pop_slice(&mut self, out: &mut [u8]) -> usize {
        let mut read = 0;
        for slot in out.iter_mut().take(self.len) {
            *slot = self.buf[self.head];
            self.head = (self.head + 1) % CONSOLE_RX_BUFFER_CAP;
            self.len -= 1;
            read += 1;
        }
        read
    }
}

struct ConsoleRxBuffer {
    borrow: AtomicU8,
    ring: UnsafeCell<ConsoleRxRing>,
}

// SAFETY: both IRQ and task contexts access the ring and the console RX register
// only after acquiring the non-blocking `borrow` gate. The IRQ path gives up
// immediately when task context owns the gate, so it cannot deadlock on input.
unsafe impl Sync for ConsoleRxBuffer {}

struct ConsoleRxBorrow<'a> {
    borrow: &'a AtomicU8,
}

impl Drop for ConsoleRxBorrow<'_> {
    fn drop(&mut self) {
        self.borrow.store(CONSOLE_RX_IDLE, Ordering::Release);
    }
}

impl ConsoleRxBuffer {
    const fn new() -> Self {
        Self {
            borrow: AtomicU8::new(CONSOLE_RX_IDLE),
            ring: UnsafeCell::new(ConsoleRxRing::new()),
        }
    }

    fn try_borrow(&self, owner: u8) -> Option<ConsoleRxBorrow<'_>> {
        self.borrow
            .compare_exchange(CONSOLE_RX_IDLE, owner, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| ConsoleRxBorrow {
                borrow: &self.borrow,
            })
    }

    fn prefetch_from_irq(&self) -> usize {
        self.with_borrowed_ring(CONSOLE_RX_IRQ_BORROW, drain_console_hardware_into_ring)
            .unwrap_or(0)
    }

    fn prefetch_from_task(&self) -> usize {
        self.with_borrowed_ring(CONSOLE_RX_TASK_BORROW, drain_console_hardware_into_ring)
            .unwrap_or(0)
    }

    fn read_from_task(&self, buf: &mut [u8]) -> usize {
        if buf.is_empty() {
            return 0;
        }

        self.with_borrowed_ring(CONSOLE_RX_TASK_BORROW, |ring| {
            let mut read = 0;
            loop {
                read += ring.pop_slice(&mut buf[read..]);
                if read == buf.len() {
                    drain_console_hardware_into_ring(ring);
                    break;
                }
                if drain_console_hardware_into_ring(ring) == 0 {
                    break;
                }
            }
            read
        })
        .unwrap_or(0)
    }

    fn with_borrowed_ring<R>(
        &self,
        owner: u8,
        f: impl FnOnce(&mut ConsoleRxRing) -> R,
    ) -> Option<R> {
        let _guard = self.try_borrow(owner)?;
        // SAFETY: `try_borrow()` must be held by the caller, which serializes
        // mutable ring access across IRQ and task contexts.
        Some(f(unsafe { &mut *self.ring.get() }))
    }
}

fn drain_console_hardware_into_ring(ring: &mut ConsoleRxRing) -> usize {
    let mut total = 0;
    let mut chunk = [0; CONSOLE_RX_DRAIN_CHUNK];
    while !ring.is_full() {
        let limit = ring.free_len().min(chunk.len());
        let read = ax_runtime::hal::console::read_bytes(&mut chunk[..limit]);
        if read == 0 {
            break;
        }
        total += ring.push_slice(&chunk[..read]);
        if read < limit {
            break;
        }
    }
    total
}

#[derive(Clone, Copy)]
pub struct Console;

#[derive(Default)]
pub struct ConsoleReader {
    mouse_filter: MouseEscapeFilter,
}

impl TtyRead for ConsoleReader {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        let mut written = 0;
        let mut raw = [0; 64];
        while written < buf.len() {
            let free = buf.len() - written;
            let pending = self.mouse_filter.pending_len();
            let read_cap = if pending < free {
                (free - pending).min(raw.len())
            } else {
                0
            };

            if read_cap == 0 {
                written += self.mouse_filter.flush_pending(&mut buf[written..]);
                break;
            }

            let read = read_console_input(&mut raw[..read_cap]);
            if read == 0 {
                written += self.mouse_filter.flush_pending(&mut buf[written..]);
                break;
            }

            written += self.mouse_filter.feed(&raw[..read], &mut buf[written..]);
            if written > 0 {
                break;
            }
        }
        written
    }
}

fn read_console_input(buf: &mut [u8]) -> usize {
    if CONSOLE_INPUT_IRQ_MODE.load(Ordering::Acquire) {
        CONSOLE_RX_BUFFER.read_from_task(buf)
    } else {
        ax_runtime::hal::console::read_bytes(buf)
    }
}

impl TtyWrite for Console {
    fn write(&self, buf: &[u8]) {
        ax_runtime::hal::console::write_bytes(buf);
    }
}

#[derive(Default)]
struct MouseEscapeFilter {
    pending: Vec<u8>,
}

enum MouseParse {
    Mouse(usize),
    NonMouse(usize),
    NeedMore,
}

enum NumberParse {
    Complete(u32),
    Invalid(usize),
    NeedMore,
}

impl MouseEscapeFilter {
    fn pending_len(&self) -> usize {
        self.pending.len()
    }

    fn feed(&mut self, input: &[u8], out: &mut [u8]) -> usize {
        self.filter(input, out, false)
    }

    #[cfg(test)]
    fn filter_chunk(&mut self, input: &[u8], out: &mut [u8]) -> usize {
        self.filter(input, out, true)
    }

    fn filter(&mut self, input: &[u8], out: &mut [u8], flush_incomplete: bool) -> usize {
        self.pending.extend_from_slice(input);

        let mut read = 0;
        let mut written = 0;
        while read < self.pending.len() {
            match parse_mouse_escape(&self.pending[read..]) {
                MouseParse::Mouse(len) => {
                    read += len;
                }
                MouseParse::NonMouse(len) => {
                    let end = read + len;
                    out[written..written + len].copy_from_slice(&self.pending[read..end]);
                    read = end;
                    written += len;
                }
                MouseParse::NeedMore => break,
            }
        }

        if read > 0 {
            self.pending.drain(..read);
        }

        if flush_incomplete {
            written += self.flush_pending(&mut out[written..]);
        }
        written
    }

    fn flush_pending(&mut self, out: &mut [u8]) -> usize {
        let len = self.pending.len().min(out.len());
        out[..len].copy_from_slice(&self.pending[..len]);
        self.pending.drain(..len);
        len
    }
}

fn parse_mouse_escape(input: &[u8]) -> MouseParse {
    if input[0] != b'\x1b' {
        return MouseParse::NonMouse(1);
    }
    if input.len() == 1 {
        return MouseParse::NeedMore;
    }
    if input[1] != b'[' {
        return MouseParse::NonMouse(2);
    }
    if input.len() == 2 {
        return MouseParse::NeedMore;
    }

    match input[2] {
        b'M' => {
            if input.len() < 6 {
                MouseParse::NeedMore
            } else {
                MouseParse::Mouse(6)
            }
        }
        b'<' => parse_sgr_mouse(input),
        b'0'..=b'9' => parse_urxvt_mouse(input),
        _ => MouseParse::NonMouse(3),
    }
}

fn parse_sgr_mouse(input: &[u8]) -> MouseParse {
    let mut pos = 3;
    for _ in 0..2 {
        match parse_number(input, pos) {
            NumberParse::Complete(_) => {}
            NumberParse::Invalid(len) => return MouseParse::NonMouse(len),
            NumberParse::NeedMore => return MouseParse::NeedMore,
        }
        while pos < input.len() && input[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos == input.len() {
            return MouseParse::NeedMore;
        }
        if input[pos] != b';' {
            return MouseParse::NonMouse(pos + 1);
        }
        pos += 1;
    }

    match parse_number(input, pos) {
        NumberParse::Complete(_) => {}
        NumberParse::Invalid(len) => return MouseParse::NonMouse(len),
        NumberParse::NeedMore => return MouseParse::NeedMore,
    }
    while pos < input.len() && input[pos].is_ascii_digit() {
        pos += 1;
    }
    if pos == input.len() {
        return MouseParse::NeedMore;
    }
    match input[pos] {
        b'M' | b'm' => MouseParse::Mouse(pos + 1),
        _ => MouseParse::NonMouse(pos + 1),
    }
}

fn parse_urxvt_mouse(input: &[u8]) -> MouseParse {
    let mut pos = 2;
    let button = match parse_number(input, pos) {
        NumberParse::Complete(value) => value,
        NumberParse::Invalid(len) => return MouseParse::NonMouse(len),
        NumberParse::NeedMore => return MouseParse::NeedMore,
    };
    for _ in 0..2 {
        while pos < input.len() && input[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos == input.len() {
            return MouseParse::NeedMore;
        }
        if input[pos] != b';' {
            return MouseParse::NonMouse(pos + 1);
        }
        pos += 1;
        match parse_number(input, pos) {
            NumberParse::Complete(_) => {}
            NumberParse::Invalid(len) => return MouseParse::NonMouse(len),
            NumberParse::NeedMore => return MouseParse::NeedMore,
        }
    }
    while pos < input.len() && input[pos].is_ascii_digit() {
        pos += 1;
    }
    if pos == input.len() {
        return MouseParse::NeedMore;
    }
    if input[pos] == b'M' && button >= 32 {
        MouseParse::Mouse(pos + 1)
    } else {
        MouseParse::NonMouse(pos + 1)
    }
}

fn parse_number(input: &[u8], start: usize) -> NumberParse {
    if start == input.len() {
        return NumberParse::NeedMore;
    }
    if !input[start].is_ascii_digit() {
        return NumberParse::Invalid(start + 1);
    }

    let mut value = 0u32;
    let mut pos = start;
    while pos < input.len() && input[pos].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add((input[pos] - b'0') as u32);
        pos += 1;
    }
    NumberParse::Complete(value)
}

/// The default TTY device.
pub static N_TTY: LazyLock<Arc<NTtyDriver>> = LazyLock::new(new_n_tty);
static CONSOLE_INPUT_SOURCE: LazyLock<Arc<PollSet>> = LazyLock::new(|| Arc::new(PollSet::new()));
static CONSOLE_INPUT_NOTIFY: LazyLock<Arc<IrqNotify>> =
    LazyLock::new(|| Arc::new(IrqNotify::new()));
static CONSOLE_RX_BUFFER: ConsoleRxBuffer = ConsoleRxBuffer::new();
static CONSOLE_INPUT_IRQ_MODE: AtomicBool = AtomicBool::new(false);
static CONSOLE_NOTIFY_WORKER: AtomicBool = AtomicBool::new(false);

fn handle_console_input_irq(_irq_num: usize) {
    let events = ax_runtime::hal::console::handle_irq();
    if events.intersects(
        ax_runtime::hal::console::ConsoleIrqEvent::RX_READY
            | ax_runtime::hal::console::ConsoleIrqEvent::RX_ERROR
            | ax_runtime::hal::console::ConsoleIrqEvent::OVERRUN,
    ) {
        CONSOLE_RX_BUFFER.prefetch_from_irq();
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
            reader: ConsoleReader::default(),
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

    CONSOLE_INPUT_IRQ_MODE.store(true, Ordering::Release);
    start_console_notify_worker();
    ax_runtime::hal::console::set_input_irq_enabled(true);
    if CONSOLE_RX_BUFFER.prefetch_from_task() > 0 {
        CONSOLE_INPUT_NOTIFY.notify();
    }
    Some(ProcessMode::InterruptDriven(CONSOLE_INPUT_SOURCE.clone()))
}

#[cfg(test)]
mod tests {
    use super::{
        CONSOLE_RX_BUFFER_CAP, ConsoleRxRing, MouseEscapeFilter, parse_console_size_response,
    };

    #[test]
    fn parses_cursor_position_response() {
        assert_eq!(
            parse_console_size_response(b"\x1b7\x1b[24;80R\x1b8"),
            Some((24, 80))
        );
    }

    fn filter(input: &[u8]) -> alloc::vec::Vec<u8> {
        let mut filter = MouseEscapeFilter::default();
        let mut out = alloc::vec![0; input.len()];
        let len = filter.filter_chunk(input, &mut out);
        out.truncate(len);
        out
    }

    #[test]
    fn mouse_filter_drops_sgr_click_wheel_and_side_button_reports() {
        assert_eq!(filter(b"\x1b[<0;10;20M"), b"");
        assert_eq!(filter(b"\x1b[<0;10;20m"), b"");
        assert_eq!(filter(b"\x1b[<64;10;20M"), b"");
        assert_eq!(filter(b"\x1b[<128;10;20M"), b"");
    }

    #[test]
    fn mouse_filter_drops_x10_report() {
        assert_eq!(filter(b"\x1b[M !!"), b"");
    }

    #[test]
    fn mouse_filter_drops_urxvt_style_report() {
        assert_eq!(filter(b"\x1b[35;10;20M"), b"");
        assert_eq!(filter(b"\x1b[96;10;20M"), b"");
    }

    #[test]
    fn mouse_filter_preserves_keyboard_and_terminal_control_sequences() {
        assert_eq!(filter(b"\x1b[A"), b"\x1b[A");
        assert_eq!(filter(b"\x1b[6n"), b"\x1b[6n");
        assert_eq!(filter(b"\x1b[1;1R"), b"\x1b[1;1R");
        assert_eq!(filter(b"\x1ba"), b"\x1ba");
    }

    #[test]
    fn mouse_filter_preserves_incomplete_or_non_mouse_sequences() {
        assert_eq!(filter(b"\x1b["), b"\x1b[");
        assert_eq!(filter(b"\x1b[M!"), b"\x1b[M!");
        assert_eq!(filter(b"\x1b[1;2;3R"), b"\x1b[1;2;3R");
        assert_eq!(filter(b"\x1b[1;2;3M"), b"\x1b[1;2;3M");
    }

    #[test]
    fn mouse_filter_removes_mouse_reports_from_mixed_stream() {
        assert_eq!(filter(b"abc \x1b[<64;10;20Mdef\n"), b"abc def\n");
    }

    #[test]
    fn console_rx_ring_preserves_prefetched_burst_order() {
        let mut ring = ConsoleRxRing::new();
        let input = b"abcdefghijklmnopqrstuvwxyz0123456789";
        assert_eq!(ring.push_slice(input), input.len());

        let mut out = [0; 64];
        let read = ring.pop_slice(&mut out);
        assert_eq!(read, input.len());
        assert_eq!(&out[..read], input);
    }

    #[test]
    fn console_rx_ring_wraps_without_reordering() {
        let mut ring = ConsoleRxRing::new();
        let first = [b'a'; CONSOLE_RX_BUFFER_CAP];
        assert_eq!(ring.push_slice(&first), first.len());

        let mut out = [0; CONSOLE_RX_BUFFER_CAP - 5];
        assert_eq!(ring.pop_slice(&mut out), out.len());

        let second = b"0123456789";
        assert_eq!(ring.push_slice(second), second.len());

        let mut rest = [0; 15];
        let read = ring.pop_slice(&mut rest);
        assert_eq!(read, rest.len());
        assert_eq!(&rest[..5], &[b'a'; 5]);
        assert_eq!(&rest[5..], second);
    }
}
