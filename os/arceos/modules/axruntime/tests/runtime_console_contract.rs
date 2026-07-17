use core::sync::atomic::{AtomicUsize, Ordering};

use ax_runtime::console::{
    RuntimeOutputFlushResultV1, RuntimeOutputResultV1, RuntimeOutputSinkV1, flush_emergency_output,
    prepare_runtime_output_sink, write_emergency_text_bytes, write_text_bytes,
};

static NORMAL_CALLS: AtomicUsize = AtomicUsize::new(0);
static NORMAL_BYTES: AtomicUsize = AtomicUsize::new(0);
static EMERGENCY_CALLS: AtomicUsize = AtomicUsize::new(0);
static EMERGENCY_BYTES: AtomicUsize = AtomicUsize::new(0);
static EMERGENCY_FLUSH_CALLS: AtomicUsize = AtomicUsize::new(0);
const LANG_ITEMS: &str = include_str!("../src/lang_items.rs");

unsafe extern "C" fn partial_then_busy(
    _context: usize,
    _bytes: *const u8,
    len: usize,
) -> RuntimeOutputResultV1 {
    let call = NORMAL_CALLS.fetch_add(1, Ordering::Relaxed);
    if call == 0 {
        let written = len.min(2);
        NORMAL_BYTES.fetch_add(written, Ordering::Relaxed);
        RuntimeOutputResultV1::progress(written)
    } else {
        RuntimeOutputResultV1::busy()
    }
}

unsafe extern "C" fn emergency(
    _context: usize,
    _bytes: *const u8,
    len: usize,
) -> RuntimeOutputResultV1 {
    EMERGENCY_CALLS.fetch_add(1, Ordering::Relaxed);
    EMERGENCY_BYTES.fetch_add(len, Ordering::Relaxed);
    RuntimeOutputResultV1::progress(len)
}

unsafe extern "C" fn emergency_flush(_context: usize) -> RuntimeOutputFlushResultV1 {
    EMERGENCY_FLUSH_CALLS.fetch_add(1, Ordering::Relaxed);
    RuntimeOutputFlushResultV1::flushed()
}

#[test]
fn committed_sink_handles_partial_busy_and_emergency_without_early_fallback() {
    NORMAL_CALLS.store(0, Ordering::Relaxed);
    NORMAL_BYTES.store(0, Ordering::Relaxed);
    EMERGENCY_CALLS.store(0, Ordering::Relaxed);
    EMERGENCY_BYTES.store(0, Ordering::Relaxed);
    EMERGENCY_FLUSH_CALLS.store(0, Ordering::Relaxed);

    let descriptor = RuntimeOutputSinkV1::new(0, partial_then_busy, emergency, emergency_flush);
    // SAFETY: both callbacks and their atomic state have process lifetime.
    let prepared = unsafe { prepare_runtime_output_sink(descriptor) }
        .expect("the first shutdown-lifetime sink must prepare");

    write_text_bytes(b"prepared");
    write_emergency_text_bytes(b"prepared-panic");
    assert_eq!(
        flush_emergency_output(),
        RuntimeOutputFlushResultV1::flushed()
    );

    assert_eq!(NORMAL_CALLS.load(Ordering::Relaxed), 2);
    assert_eq!(NORMAL_BYTES.load(Ordering::Relaxed), 2);
    assert_eq!(EMERGENCY_CALLS.load(Ordering::Relaxed), 1);
    assert_eq!(EMERGENCY_BYTES.load(Ordering::Relaxed), 14);
    assert_eq!(EMERGENCY_FLUSH_CALLS.load(Ordering::Relaxed), 1);

    NORMAL_CALLS.store(0, Ordering::Relaxed);
    NORMAL_BYTES.store(0, Ordering::Relaxed);
    EMERGENCY_CALLS.store(0, Ordering::Relaxed);
    EMERGENCY_BYTES.store(0, Ordering::Relaxed);
    EMERGENCY_FLUSH_CALLS.store(0, Ordering::Relaxed);
    prepared
        .commit()
        .expect("a live prepared sink token must commit");

    write_text_bytes(b"normal");
    write_emergency_text_bytes(b"panic");
    assert_eq!(
        flush_emergency_output(),
        RuntimeOutputFlushResultV1::flushed()
    );

    assert_eq!(NORMAL_CALLS.load(Ordering::Relaxed), 2);
    assert_eq!(NORMAL_BYTES.load(Ordering::Relaxed), 2);
    assert_eq!(EMERGENCY_CALLS.load(Ordering::Relaxed), 1);
    assert_eq!(EMERGENCY_BYTES.load(Ordering::Relaxed), 5);
    assert_eq!(EMERGENCY_FLUSH_CALLS.load(Ordering::Relaxed), 1);
}

#[test]
fn recursive_panic_uses_only_a_fixed_emergency_diagnostic() {
    let recursive = LANG_ITEMS
        .split_once("axpanic::PanicDisposition::Recursive =>")
        .expect("recursive panic arm")
        .1
        .split_once("axpanic::PanicDisposition::Concurrent =>")
        .expect("concurrent panic arm")
        .0;

    assert!(recursive.contains("recursive kernel panic"));
    assert!(recursive.contains("panic_shutdown()"));
    assert!(!recursive.contains("panic_backtrace"));
    assert!(!recursive.contains("write_current_raw"));
    assert!(LANG_ITEMS.contains("crate::console::flush_emergency_output()"));
}
