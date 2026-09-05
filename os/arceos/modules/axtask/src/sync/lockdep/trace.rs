use core::{
    fmt::{self, Write},
    panic::Location,
    ptr::{self, addr_of, addr_of_mut},
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};

#[cfg(any(test, doctest, not(target_arch = "riscv64")))]
#[path = "trace/dummy.rs"]
mod dummy;
#[cfg(all(target_arch = "riscv64", not(any(test, doctest))))]
#[path = "trace/riscv64.rs"]
mod riscv64;

const TRACE_BUFFER_CAP: usize = 65536;
const TRACE_EVENT_TMP_CAP: usize = 192;
const IRQSAVE_LOCK_OBSERVATION_CAP: usize = 2048;
const IRQSAVE_KIND_NONE: u8 = 0;
const IRQSAVE_KIND_SPIN: u8 = 1;
const IRQSAVE_KIND_RWLOCK: u8 = 2;
static TRACE_ENABLED: AtomicBool = AtomicBool::new(false);
static TRACE_EVENT_SEQ: AtomicUsize = AtomicUsize::new(0);
static TRACE_TRUNCATED: AtomicBool = AtomicBool::new(false);
static TRACE_LEN: AtomicUsize = AtomicUsize::new(0);
static IRQSAVE_OBSERVATIONS_TRUNCATED: AtomicBool = AtomicBool::new(false);
static mut TRACE_BUFFER: [u8; TRACE_BUFFER_CAP] = [0; TRACE_BUFFER_CAP];
static IRQSAVE_LOCK_OBSERVATIONS: [IrqsaveLockObservation; IRQSAVE_LOCK_OBSERVATION_CAP] =
    [const { IrqsaveLockObservation::new() }; IRQSAVE_LOCK_OBSERVATION_CAP];

struct IrqsaveLockObservation {
    addr: AtomicUsize,
    kind: AtomicU8,
    class_key: AtomicUsize,
    caller: AtomicUsize,
    seen_non_irq: AtomicBool,
    seen_irq: AtomicBool,
}

impl IrqsaveLockObservation {
    const fn new() -> Self {
        Self {
            addr: AtomicUsize::new(0),
            kind: AtomicU8::new(IRQSAVE_KIND_NONE),
            class_key: AtomicUsize::new(0),
            caller: AtomicUsize::new(0),
            seen_non_irq: AtomicBool::new(false),
            seen_irq: AtomicBool::new(false),
        }
    }

    fn reset(&self) {
        self.addr.store(0, Ordering::Relaxed);
        self.kind.store(IRQSAVE_KIND_NONE, Ordering::Relaxed);
        self.class_key.store(0, Ordering::Relaxed);
        self.caller.store(0, Ordering::Relaxed);
        self.seen_non_irq.store(false, Ordering::Relaxed);
        self.seen_irq.store(false, Ordering::Relaxed);
    }
}

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

pub(crate) fn set_trace_enabled(enabled: bool) {
    if enabled {
        TRACE_EVENT_SEQ.store(0, Ordering::Relaxed);
        TRACE_LEN.store(0, Ordering::Relaxed);
        TRACE_TRUNCATED.store(false, Ordering::Relaxed);
        reset_irqsave_lock_observations();
    }
    TRACE_ENABLED.store(enabled, Ordering::Relaxed);
}

pub(crate) fn dump_trace_buffer() {
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
    dump_irqsave_lock_observations();
}

fn reset_irqsave_lock_observations() {
    IRQSAVE_OBSERVATIONS_TRUNCATED.store(false, Ordering::Relaxed);
    for observation in &IRQSAVE_LOCK_OBSERVATIONS {
        observation.reset();
    }
}

fn current_irq_context() -> bool {
    #[cfg(feature = "irq")]
    {
        ax_hal::irq::in_irq_context()
    }
    #[cfg(not(feature = "irq"))]
    {
        false
    }
}

pub(crate) fn observe_irqsave_lock_acquire(
    kind: &str,
    addr: usize,
    class_key: usize,
    caller: &'static Location<'static>,
) {
    if !TRACE_ENABLED.load(Ordering::Relaxed) || addr == 0 {
        return;
    }

    let kind = match kind {
        "spin" => IRQSAVE_KIND_SPIN,
        "spin-rwlock" => IRQSAVE_KIND_RWLOCK,
        _ => return,
    };
    let in_irq = current_irq_context();

    for observation in &IRQSAVE_LOCK_OBSERVATIONS {
        let observed_addr = observation.addr.load(Ordering::Acquire);
        if observed_addr == addr
            || observed_addr == 0
                && observation
                    .addr
                    .compare_exchange(0, addr, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
        {
            observation.kind.store(kind, Ordering::Release);
            observation.class_key.store(class_key, Ordering::Release);
            observation.caller.store(
                caller as *const Location<'static> as usize,
                Ordering::Release,
            );
            if in_irq {
                observation.seen_irq.store(true, Ordering::Release);
            } else {
                observation.seen_non_irq.store(true, Ordering::Release);
            }
            return;
        }
    }

    IRQSAVE_OBSERVATIONS_TRUNCATED.store(true, Ordering::Relaxed);
}

fn dump_irqsave_lock_observations() {
    let mut found = false;
    for observation in &IRQSAVE_LOCK_OBSERVATIONS {
        let addr = observation.addr.load(Ordering::Acquire);
        if addr == 0
            || !observation.seen_non_irq.load(Ordering::Acquire)
            || observation.seen_irq.load(Ordering::Acquire)
        {
            continue;
        }

        if !found {
            emit_str("lockdep: irqsave locks never observed in IRQ context:\n");
            found = true;
        }

        emit_irqsave_lock_observation(observation, addr);
    }

    if found && IRQSAVE_OBSERVATIONS_TRUNCATED.load(Ordering::Relaxed) {
        emit_str("lockdep: irqsave lock observation table truncated\n");
    }
}

fn emit_irqsave_lock_observation(observation: &IrqsaveLockObservation, addr: usize) {
    let mut writer = EventWriter::new();
    let kind = match observation.kind.load(Ordering::Acquire) {
        IRQSAVE_KIND_SPIN => "spin",
        IRQSAVE_KIND_RWLOCK => "spin-rwlock",
        _ => "unknown",
    };
    let _ = writer.write_fmt(format_args!("  {kind} addr={addr:#x}"));
    write_location(
        &mut writer,
        " caller",
        observation.caller.load(Ordering::Acquire),
    );
    write_location(
        &mut writer,
        " class",
        observation.class_key.load(Ordering::Acquire),
    );
    let _ = writer.write_char('\n');
    emit_str(core::str::from_utf8(writer.as_bytes()).unwrap_or(""));
}

fn write_location(writer: &mut EventWriter, label: &str, location: usize) {
    if location == 0 {
        let _ = writer.write_fmt(format_args!(" {label}=<dynamic>"));
        return;
    }

    // SAFETY: lock metadata and caller locations are stored as `'static`
    // `Location` pointers by the lock constructors and acquisition wrappers.
    let location = unsafe { &*(location as *const Location<'static>) };
    let _ = writer.write_fmt(format_args!(
        " {label}={}:{}:{}",
        location.file(),
        location.line(),
        location.column()
    ));
}

pub(crate) fn trace_lock_begin(kind: &str, addr: usize, is_try: bool, detail: Option<&str>) {
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

pub(crate) fn trace_lock_finish(
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

pub(crate) fn trace_unlock(kind: &str, addr: usize, detail: Option<&str>) {
    if let Some(detail) = detail {
        trace_event(
            kind,
            format_args!("{kind} unlock {detail} addr={:#x}", addr),
        );
    } else {
        trace_event(kind, format_args!("{kind} unlock addr={:#x}", addr));
    }
}
