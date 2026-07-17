//! Runtime-owned console output routing.
//!
//! Platform console handover and output routing are separate ownership
//! transitions. The platform token retires the early UART register owner,
//! while this module publishes the writer that owns the runtime device. Once a
//! runtime writer is committed, output must never fall back to the early path.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU8, Ordering},
};

const RUNTIME_OUTPUT_ABI_V1: u16 = 1;
const OUTPUT_EMPTY: u8 = 0;
const OUTPUT_INSTALLING: u8 = 1;
const OUTPUT_PREPARED: u8 = 2;
const OUTPUT_COMMITTED: u8 = 3;
const OUTPUT_FAILED_CLOSED: u8 = 4;
const OUTPUT_CALLBACK_PROGRESS: u8 = 0;
const OUTPUT_CALLBACK_BUSY: u8 = 1;
const OUTPUT_CALLBACK_FAILED: u8 = 2;
const OUTPUT_FLUSHED: u8 = 0;
const OUTPUT_FLUSH_BUSY: u8 = 1;
const OUTPUT_FLUSH_FAILED: u8 = 2;
const MAX_OUTPUT_CALLBACK_BYTES: usize = 256;
const MAX_OUTPUT_CALLBACK_CALLS: usize = 64;

/// Runtime output callback ABI.
///
/// The callback must not allocate, block, sleep, or invoke the early platform
/// console. It may accept a prefix and report [`RuntimeOutputResultV1::busy`]
/// when its preallocated queue has no room.
pub type RuntimeOutputCallbackV1 =
    unsafe extern "C" fn(context: usize, bytes: *const u8, len: usize) -> RuntimeOutputResultV1;
/// Runtime emergency-drain callback ABI.
pub type RuntimeOutputFlushCallbackV1 =
    unsafe extern "C" fn(context: usize) -> RuntimeOutputFlushResultV1;

/// Result returned by a runtime output callback.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeOutputResultV1 {
    written: usize,
    status: u8,
    reserved: [u8; 7],
}

impl RuntimeOutputResultV1 {
    /// Reports that `written` bytes were accepted.
    pub const fn progress(written: usize) -> Self {
        Self {
            written,
            status: OUTPUT_CALLBACK_PROGRESS,
            reserved: [0; 7],
        }
    }

    /// Reports that the fixed-capacity runtime writer is currently busy.
    pub const fn busy() -> Self {
        Self {
            written: 0,
            status: OUTPUT_CALLBACK_BUSY,
            reserved: [0; 7],
        }
    }

    /// Reports a permanent writer failure for this output attempt.
    pub const fn failed() -> Self {
        Self {
            written: 0,
            status: OUTPUT_CALLBACK_FAILED,
            reserved: [0; 7],
        }
    }
}

/// Result returned by a bounded runtime emergency-drain callback.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeOutputFlushResultV1 {
    status: u8,
    reserved: [u8; 7],
}

impl RuntimeOutputFlushResultV1 {
    /// Reports that the UART transmitter became idle.
    pub const fn flushed() -> Self {
        Self {
            status: OUTPUT_FLUSHED,
            reserved: [0; 7],
        }
    }

    /// Reports owner contention or exhaustion of the fixed status-read budget.
    pub const fn busy() -> Self {
        Self {
            status: OUTPUT_FLUSH_BUSY,
            reserved: [0; 7],
        }
    }

    /// Reports that no trustworthy runtime transmitter is available.
    pub const fn failed() -> Self {
        Self {
            status: OUTPUT_FLUSH_FAILED,
            reserved: [0; 7],
        }
    }
}

/// Shutdown-lifetime runtime output descriptor.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RuntimeOutputSinkV1 {
    abi_version: u16,
    reserved: [u8; 6],
    context: usize,
    normal: RuntimeOutputCallbackV1,
    emergency: RuntimeOutputCallbackV1,
    emergency_flush: RuntimeOutputFlushCallbackV1,
}

impl RuntimeOutputSinkV1 {
    /// Creates a version-one runtime output descriptor.
    pub const fn new(
        context: usize,
        normal: RuntimeOutputCallbackV1,
        emergency: RuntimeOutputCallbackV1,
        emergency_flush: RuntimeOutputFlushCallbackV1,
    ) -> Self {
        Self {
            abi_version: RUNTIME_OUTPUT_ABI_V1,
            reserved: [0; 6],
            context,
            normal,
            emergency,
            emergency_flush,
        }
    }
}

/// Failure to prepare or commit a runtime output sink.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RuntimeOutputSinkError {
    /// Another descriptor is being installed or is already committed.
    #[error("a runtime output sink is already active")]
    Busy,
    /// The descriptor does not implement the supported ABI.
    #[error("the runtime output sink descriptor is invalid")]
    InvalidDescriptor,
    /// The prepared descriptor lost its one-shot installation state.
    #[error("the runtime output sink token is stale")]
    InvalidToken,
}

/// Prepared one-shot publication of a runtime output sink.
#[must_use = "dropping the token aborts runtime output publication"]
pub struct PreparedRuntimeOutputSink {
    active: bool,
}

impl PreparedRuntimeOutputSink {
    /// Publishes the prepared sink for the rest of the system lifetime.
    pub fn commit(mut self) -> Result<(), RuntimeOutputSinkError> {
        // A failed commit must not let Drop clear a descriptor another owner
        // could have installed after observing the failure.
        self.active = false;
        let result = RUNTIME_OUTPUT_STATE.compare_exchange(
            OUTPUT_PREPARED,
            OUTPUT_COMMITTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        if result.is_err() {
            // The platform handover may already have irreversibly retired the
            // early register owner. Unknown publication state must therefore
            // fail closed instead of permitting a fallback to that owner.
            RUNTIME_OUTPUT_STATE.store(OUTPUT_FAILED_CLOSED, Ordering::Release);
            return Err(RuntimeOutputSinkError::InvalidToken);
        }
        Ok(())
    }

    /// Irreversibly disables early fallback after runtime takeover recovery
    /// could not prove a writable runtime console.
    pub fn fail_closed(mut self) {
        self.active = false;
        if RUNTIME_OUTPUT_STATE
            .compare_exchange(
                OUTPUT_PREPARED,
                OUTPUT_FAILED_CLOSED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            RUNTIME_OUTPUT_STATE.store(OUTPUT_FAILED_CLOSED, Ordering::Release);
        }
    }
}

impl Drop for PreparedRuntimeOutputSink {
    fn drop(&mut self) {
        if self.active {
            let _ = RUNTIME_OUTPUT_STATE.compare_exchange(
                OUTPUT_PREPARED,
                OUTPUT_EMPTY,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

struct RuntimeOutputStorage(UnsafeCell<MaybeUninit<RuntimeOutputSinkV1>>);

// SAFETY: writers own the OUTPUT_INSTALLING state exclusively. Readers access
// the immutable descriptor only after the OUTPUT_PREPARED Release/Acquire
// publication, and no descriptor is replaced while PREPARED or COMMITTED.
unsafe impl Sync for RuntimeOutputStorage {}

static RUNTIME_OUTPUT_STATE: AtomicU8 = AtomicU8::new(OUTPUT_EMPTY);
static RUNTIME_OUTPUT: RuntimeOutputStorage =
    RuntimeOutputStorage(UnsafeCell::new(MaybeUninit::uninit()));

/// Prepares a shutdown-lifetime runtime output sink.
///
/// # Safety
///
/// `descriptor.context` and all callbacks must remain valid until shutdown.
/// The write callbacks must accept the supplied readable byte slice. Every
/// callback must be safe in hard-IRQ and panic context and must not allocate,
/// block, sleep, call user code, or access the retired early platform console.
pub unsafe fn prepare_runtime_output_sink(
    descriptor: RuntimeOutputSinkV1,
) -> Result<PreparedRuntimeOutputSink, RuntimeOutputSinkError> {
    if descriptor.abi_version != RUNTIME_OUTPUT_ABI_V1 || descriptor.reserved != [0; 6] {
        return Err(RuntimeOutputSinkError::InvalidDescriptor);
    }
    RUNTIME_OUTPUT_STATE
        .compare_exchange(
            OUTPUT_EMPTY,
            OUTPUT_INSTALLING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| RuntimeOutputSinkError::Busy)?;

    // SAFETY: OUTPUT_INSTALLING has exactly one owner, and readers require the
    // later OUTPUT_PREPARED publication before accessing this storage.
    unsafe { (*RUNTIME_OUTPUT.0.get()).write(descriptor) };
    RUNTIME_OUTPUT_STATE.store(OUTPUT_PREPARED, Ordering::Release);
    Ok(PreparedRuntimeOutputSink { active: true })
}

/// Writes text through the current console owner, expanding LF to CRLF.
pub fn write_text_bytes(bytes: &[u8]) {
    match output_route() {
        OutputRoute::Early => ax_hal::console::write_text_bytes(bytes),
        OutputRoute::Runtime(sink) => write_runtime_text(bytes, sink.context, sink.normal),
        OutputRoute::FailedClosed => {}
    }
}

/// Writes panic/fatal text without entering a blocking or allocating path.
pub fn write_emergency_text_bytes(bytes: &[u8]) {
    match output_route() {
        OutputRoute::Early => ax_hal::console::write_text_bytes(bytes),
        OutputRoute::Runtime(sink) => write_runtime_text(bytes, sink.context, sink.emergency),
        OutputRoute::FailedClosed => {}
    }
}

/// Makes one bounded attempt to drain the runtime emergency transmitter.
///
/// This function never loops on a busy callback and never falls back to the
/// early platform owner. Before runtime publication there is no portable drain
/// capability, so it reports `failed`.
pub fn flush_emergency_output() -> RuntimeOutputFlushResultV1 {
    let OutputRoute::Runtime(sink) = output_route() else {
        return RuntimeOutputFlushResultV1::failed();
    };
    // SAFETY: the descriptor contract was accepted by the unsafe prepare call.
    let result = unsafe { (sink.emergency_flush)(sink.context) };
    if result.reserved != [0; 7]
        || !matches!(
            result.status,
            OUTPUT_FLUSHED | OUTPUT_FLUSH_BUSY | OUTPUT_FLUSH_FAILED
        )
    {
        return RuntimeOutputFlushResultV1::failed();
    }
    result
}

enum OutputRoute {
    Early,
    Runtime(RuntimeOutputSinkV1),
    FailedClosed,
}

fn output_route() -> OutputRoute {
    match RUNTIME_OUTPUT_STATE.load(Ordering::Acquire) {
        OUTPUT_EMPTY | OUTPUT_INSTALLING => OutputRoute::Early,
        OUTPUT_PREPARED | OUTPUT_COMMITTED => {
            // SAFETY: both states Acquire-observe descriptor initialization;
            // the descriptor is Copy and immutable until shutdown. PREPARED
            // deliberately routes to runtime so a paused early owner can never
            // be called during the hardware handover window.
            OutputRoute::Runtime(unsafe { *(*RUNTIME_OUTPUT.0.get()).assume_init_ref() })
        }
        OUTPUT_FAILED_CLOSED => OutputRoute::FailedClosed,
        _ => OutputRoute::FailedClosed,
    }
}

fn write_runtime_text(bytes: &[u8], context: usize, callback: RuntimeOutputCallbackV1) {
    let mut calls_remaining = MAX_OUTPUT_CALLBACK_CALLS;
    let mut start = 0;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if calls_remaining == 0 {
            return;
        }
        if byte == b'\n' {
            if start < index
                && !write_runtime_bytes(
                    &bytes[start..index],
                    context,
                    callback,
                    &mut calls_remaining,
                )
            {
                return;
            }
            if !write_runtime_bytes(b"\r\n", context, callback, &mut calls_remaining) {
                return;
            }
            start = index + 1;
        }
    }
    if start < bytes.len() {
        let _ = write_runtime_bytes(&bytes[start..], context, callback, &mut calls_remaining);
    }
}

fn write_runtime_bytes(
    mut bytes: &[u8],
    context: usize,
    callback: RuntimeOutputCallbackV1,
    calls_remaining: &mut usize,
) -> bool {
    while !bytes.is_empty() && *calls_remaining > 0 {
        let chunk_len = bytes.len().min(MAX_OUTPUT_CALLBACK_BYTES);
        let chunk = &bytes[..chunk_len];
        *calls_remaining -= 1;
        // SAFETY: the descriptor contract was accepted by the unsafe prepare
        // call, and `chunk` remains readable for the duration of the callback.
        let result = unsafe { callback(context, chunk.as_ptr(), chunk.len()) };
        if result.status != OUTPUT_CALLBACK_PROGRESS
            || result.reserved != [0; 7]
            || result.written == 0
            || result.written > chunk.len()
        {
            return false;
        }
        bytes = &bytes[result.written..];
    }
    bytes.is_empty()
}
