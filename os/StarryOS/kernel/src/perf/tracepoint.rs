//! Tracepoint perf event. The user attaches a BPF program to a static
//! tracepoint identified by its numeric id (from
//! `/sys/kernel/debug/tracing/events/<sys>/<event>/id`).
//!
//! Adapted from `Starry-OS/StarryOS:ebpf-kmod` (`kernel/src/perf/tracepoint.rs`)
//! to use ktracepoint **0.6**:
//!
//! * ktracepoint 0.6 dropped the `TracePoint<L, K>` lock parameter — there
//!   is a single generic over `K: KernelTraceOps`, and `ExtTracePoint<K>`
//!   wraps callback management.
//! * `TraceEventFunc::new(closure, data)` replaces the trait-object based
//!   `TracePointCallBackFunc::call(entry)` callback registration.
//! * Registration goes through `ExtTracePoint::register(TraceCallbackType::Event(...))`
//!   rather than `TracePoint::register_event_callback(id, callback)`.
//! * Enable/disable is implicit: `ExtTracePoint::register` enables the
//!   static-key when the callback list becomes non-empty.

use alloc::{boxed::Box, sync::Arc};
use core::any::Any;

use ax_errno::{AxError, AxResult};
use axpoll::Pollable;
use kbpf_basic::perf::{PerfProbeArgs, PerfProbeConfig};
use ktracepoint::{TraceCallbackType, TraceEventFunc};

use crate::{
    file::FileLike,
    perf::{PerfEventOps, bpf::OwnedEbpfVm},
    tracepoint::{KernelExtTracePoint, lookup_ext_tracepoint},
};

/// Closure signature accepted by `TraceEventFunc::new` for cooked tracepoints:
/// the tracing layer hands over the per-cpu sample bytes plus the type-erased
/// per-callback payload, and the closure dispatches into the BPF VM.
type TpCallback = Box<dyn Fn(&[u8], &(dyn Any + Send + Sync)) + Send + Sync>;

/// Per-fd tracepoint perf event. Holds the Arc<Mutex<ExtTracePoint>> so we
/// can register/unregister callbacks on drop; remembers the callback
/// payload so the same registration can be undone (ktracepoint 0.6
/// `unregister(callback)` compares Arc pointer identity).
pub struct TracepointPerfEvent {
    _args: PerfProbeArgs,
    ext_tp: KernelExtTracePoint,
    registered: alloc::vec::Vec<Arc<TraceEventFunc>>,
}

impl core::fmt::Debug for TracepointPerfEvent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TracepointPerfEvent").finish()
    }
}

impl TracepointPerfEvent {
    /// Create a perf event for the given resolved tracepoint.
    pub fn new(args: PerfProbeArgs, ext_tp: KernelExtTracePoint) -> Self {
        Self {
            _args: args,
            ext_tp,
            registered: alloc::vec::Vec::new(),
        }
    }
}

impl Pollable for TracepointPerfEvent {
    fn poll(&self) -> axpoll::IoEvents {
        axpoll::IoEvents::empty()
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: axpoll::IoEvents) {
        // Tracepoint perf events do not deliver readiness through poll;
        // sample delivery is via the attached BPF program or trace_pipe.
    }
}

impl PerfEventOps for TracepointPerfEvent {
    fn set_bpf_prog(&mut self, bpf_prog: Arc<dyn FileLike>) -> AxResult<()> {
        // `OwnedEbpfVm` bundles the rbpf interpreter with the `Arc<BpfProg>`
        // that backs its instruction slice (drop order is field-order, so
        // the borrower dies before the buffer). `execute_program` runs off
        // `&self`, so the VM is driven directly from the `&dyn Any` the
        // `TraceEventFunc` closure receives — no lock required.
        struct Ctx {
            vm: OwnedEbpfVm,
        }
        let ctx = Box::new(Ctx {
            vm: OwnedEbpfVm::new(bpf_prog)?,
        });

        let func: TpCallback = Box::new(|entry: &[u8], data: &(dyn Any + Send + Sync)| {
            // `TraceEventFunc` keeps the payload as `Box<dyn Any + Send + Sync>`
            // and hands the closure `&self.data`, so the concrete type observed
            // here is the *box*, not `Ctx` (same as the raw-tracepoint path in
            // `raw_tracepoint.rs`). Downcast through the box first.
            let ctx = data
                .downcast_ref::<Box<dyn Any + Send + Sync>>()
                .and_then(|boxed| boxed.downcast_ref::<Ctx>())
                .expect("tracepoint Ctx mismatch");
            // BPF programs expect a mutable context slice; the
            // tracepoint hands us a `&[u8]` carved out of its
            // per-cpu sample buffer, which is single-writer at that
            // point, so casting to `&mut [u8]` is safe under the
            // tracepoint contract.
            let entry =
                unsafe { core::slice::from_raw_parts_mut(entry.as_ptr() as *mut u8, entry.len()) };
            if let Err(e) = ctx.vm.execute_program(entry) {
                error!("tracepoint BPF program failed: {e:?}");
            }
        });
        let callback = Arc::new(TraceEventFunc::new(func, ctx));
        self.ext_tp
            .lock()
            .register(TraceCallbackType::Event(callback.clone()));
        self.registered.push(callback);
        Ok(())
    }

    fn enable(&mut self) -> AxResult<()> {
        // ktracepoint dispatch only invokes a cooked `TraceEventFunc` when
        // its per-callback `perf_enabled` flag is set (see ktracepoint 0.6
        // `basic_macro.rs`), and `TraceEventFunc::new` starts disabled. So a
        // perf event that is registered but not enabled would silently never
        // fire — we must flip the flag on every callback we registered.
        for cb in &self.registered {
            cb.set_perf_enable(true);
        }
        Ok(())
    }

    fn disable(&mut self) -> AxResult<()> {
        for cb in &self.registered {
            cb.set_perf_enable(false);
        }
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Drop for TracepointPerfEvent {
    fn drop(&mut self) {
        let mut ext_tp = self.ext_tp.lock();
        for cb in self.registered.drain(..) {
            ext_tp.unregister(TraceCallbackType::Event(cb));
        }
    }
}

/// Build a tracepoint perf event from `perf_event_open` args. The config
/// field carries the numeric tracepoint id (the same value debugfs
/// `events/<sys>/<event>/id` reports).
pub fn perf_event_open_tracepoint(args: PerfProbeArgs) -> AxResult<TracepointPerfEvent> {
    let tp_id = match args.config {
        PerfProbeConfig::Raw(id) => id as u32,
        _ => return Err(AxError::InvalidInput),
    };
    let ext_tp = lookup_ext_tracepoint(tp_id).ok_or(AxError::NotFound)?;
    Ok(TracepointPerfEvent::new(args, ext_tp))
}
