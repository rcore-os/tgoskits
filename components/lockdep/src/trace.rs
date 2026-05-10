use core::{
    fmt::{self, Write},
    ptr::{self, addr_of, addr_of_mut},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[cfg(any(test, doctest, not(target_arch = "riscv64")))]
mod dummy;
#[cfg(all(target_arch = "riscv64", not(any(test, doctest))))]
mod riscv64;

const TRACE_BUFFER_CAP: usize = 65536;
const TRACE_EVENT_TMP_CAP: usize = 192;
static TRACE_ENABLED: AtomicBool = AtomicBool::new(false);
static TRACE_EVENT_SEQ: AtomicUsize = AtomicUsize::new(0);
static TRACE_TRUNCATED: AtomicBool = AtomicBool::new(false);
static TRACE_LEN: AtomicUsize = AtomicUsize::new(0);
static mut TRACE_BUFFER: [u8; TRACE_BUFFER_CAP] = [0; TRACE_BUFFER_CAP];

struct EventWriter {
    buf: [u8; TRACE_EVENT_TMP_CAP],
    len: usize,
}

impl EventWriter {
    const fn new() -> Self {
        Self {
            buf: [0; TRACE_EVENT_TMP_CAP],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Write for EventWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let remaining = TRACE_EVENT_TMP_CAP.saturating_sub(self.len);
        let write_len = remaining.min(bytes.len());
        self.buf[self.len..self.len + write_len].copy_from_slice(&bytes[..write_len]);
        self.len += write_len;
        if write_len == bytes.len() {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

fn emit_str(s: &str) {
    for byte in s.bytes() {
        emit_byte(byte);
    }
}

fn emit_byte(byte: u8) {
    if byte == b'\n' {
        backend_emit_byte(b'\r');
    }
    backend_emit_byte(byte);
}

#[cfg(all(target_arch = "riscv64", not(any(test, doctest))))]
fn backend_emit_byte(byte: u8) {
    riscv64::emit_byte(byte);
}

#[cfg(any(test, doctest, not(target_arch = "riscv64")))]
fn backend_emit_byte(byte: u8) {
    dummy::emit_byte(byte);
}

fn trace_buffer_write(bytes: &[u8]) {
    if !TRACE_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    let start = TRACE_LEN.fetch_add(bytes.len(), Ordering::Relaxed);
    if start >= TRACE_BUFFER_CAP {
        TRACE_TRUNCATED.store(true, Ordering::Relaxed);
        return;
    }

    let end = (start + bytes.len()).min(TRACE_BUFFER_CAP);
    let copy_len = end - start;
    // SAFETY: `start..end` is uniquely reserved by the atomic fetch_add above.
    unsafe {
        ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            addr_of_mut!(TRACE_BUFFER).cast::<u8>().add(start),
            copy_len,
        );
    }
    if copy_len != bytes.len() {
        TRACE_TRUNCATED.store(true, Ordering::Relaxed);
    }
}

fn trace_event(kind: &str, args: fmt::Arguments<'_>) {
    if !TRACE_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    let mut writer = EventWriter::new();
    let seq = TRACE_EVENT_SEQ.fetch_add(1, Ordering::Relaxed);
    let _ = writer.write_fmt(format_args!("[lockdep:{kind}:{seq:03}] "));
    let _ = writer.write_fmt(args);
    let _ = writer.write_char('\n');
    trace_buffer_write(writer.as_bytes());
}

pub fn set_trace_enabled(enabled: bool) {
    if enabled {
        TRACE_EVENT_SEQ.store(0, Ordering::Relaxed);
        TRACE_LEN.store(0, Ordering::Relaxed);
        TRACE_TRUNCATED.store(false, Ordering::Relaxed);
    }
    TRACE_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn dump_trace_buffer() {
    let len = TRACE_LEN.load(Ordering::Relaxed).min(TRACE_BUFFER_CAP);
    if len != 0 {
        // SAFETY: reading a prefix of the static buffer after tracing is disabled.
        let bytes =
            unsafe { core::slice::from_raw_parts(addr_of!(TRACE_BUFFER).cast::<u8>(), len) };
        emit_str(core::str::from_utf8(bytes).unwrap_or("<lockdep trace utf8 error>\n"));
    }
    if TRACE_TRUNCATED.load(Ordering::Relaxed) {
        emit_str("lockdep: trace truncated\n");
    }
}

pub fn trace_lock_begin(kind: &str, addr: usize, is_try: bool, detail: Option<&str>) {
    if let Some(detail) = detail {
        trace_event(
            kind,
            format_args!(
                "{} {} {} addr={:#x}",
                kind,
                if is_try { "try_lock" } else { "lock" },
                detail,
                addr
            ),
        );
    } else {
        trace_event(
            kind,
            format_args!(
                "{} {} addr={:#x}",
                kind,
                if is_try { "try_lock" } else { "lock" },
                addr
            ),
        );
    }
}

pub fn trace_lock_finish(
    kind: &str,
    addr: usize,
    is_try: bool,
    acquired: bool,
    detail: Option<&str>,
) {
    if let Some(detail) = detail {
        trace_event(
            kind,
            format_args!(
                "{} {} {} {} addr={:#x}",
                kind,
                if is_try { "try_lock" } else { "lock" },
                if acquired { "ok" } else { "fail" },
                detail,
                addr
            ),
        );
    } else {
        trace_event(
            kind,
            format_args!(
                "{} {} {} addr={:#x}",
                kind,
                if is_try { "try_lock" } else { "lock" },
                if acquired { "ok" } else { "fail" },
                addr
            ),
        );
    }
}

pub fn trace_unlock(kind: &str, addr: usize, detail: Option<&str>) {
    if let Some(detail) = detail {
        trace_event(
            kind,
            format_args!("{kind} unlock {detail} addr={:#x}", addr),
        );
    } else {
        trace_event(kind, format_args!("{kind} unlock addr={:#x}", addr));
    }
}
