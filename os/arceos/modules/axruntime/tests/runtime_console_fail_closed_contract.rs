use core::sync::atomic::{AtomicUsize, Ordering};

use ax_runtime::console::{
    RuntimeOutputFlushResultV1, RuntimeOutputResultV1, RuntimeOutputSinkV1, flush_emergency_output,
    prepare_runtime_output_sink, write_emergency_text_bytes, write_text_bytes,
};

static CALLBACK_CALLS: AtomicUsize = AtomicUsize::new(0);
const CONSOLE: &str = include_str!("../src/console.rs");

unsafe extern "C" fn unexpected_callback(
    _context: usize,
    _bytes: *const u8,
    len: usize,
) -> RuntimeOutputResultV1 {
    CALLBACK_CALLS.fetch_add(1, Ordering::Relaxed);
    RuntimeOutputResultV1::progress(len)
}

unsafe extern "C" fn unexpected_flush(_context: usize) -> RuntimeOutputFlushResultV1 {
    CALLBACK_CALLS.fetch_add(1, Ordering::Relaxed);
    RuntimeOutputFlushResultV1::flushed()
}

#[test]
fn failed_closed_runtime_output_drops_without_invoking_a_writer() {
    CALLBACK_CALLS.store(0, Ordering::Relaxed);
    let descriptor = RuntimeOutputSinkV1::new(
        0,
        unexpected_callback,
        unexpected_callback,
        unexpected_flush,
    );
    // SAFETY: the callback and atomic state have process lifetime.
    let prepared = unsafe { prepare_runtime_output_sink(descriptor) }
        .expect("the first shutdown-lifetime sink must prepare");

    prepared.fail_closed();
    write_text_bytes(b"normal");
    write_emergency_text_bytes(b"panic");
    assert_eq!(
        flush_emergency_output(),
        RuntimeOutputFlushResultV1::failed()
    );

    assert_eq!(CALLBACK_CALLS.load(Ordering::Relaxed), 0);
    assert!(CONSOLE.contains("OutputRoute::Early => ax_hal::console::write_text_bytes(bytes)"));
    assert!(CONSOLE.contains("OutputRoute::FailedClosed => {}"));
}
