//! A Rust library for defining and managing tracepoints in a no_std environment.
//! It provides macros and structures to create tracepoints, manage their state,
//! and handle trace events efficiently.
//! The library is designed to be lightweight and suitable for embedded systems or
//! kernel-level programming where the standard library is not available.
//! It leverages Rust's powerful macro system to simplify the creation and management of tracepoints.
//! The macros provided by this library allow for easy insertion of tracepoints into code with minimal overhead.
#![deny(missing_docs)]
#![no_std]
#![allow(clippy::new_without_default)]
extern crate alloc;

mod basic_macro;
mod point;
pub mod ptr;
mod trace_pipe;

use alloc::{
    boxed::Box,
    collections::BTreeMap,
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    ops::{Deref, DerefMut},
    sync::atomic::AtomicU32,
};

pub use paste;
pub use point::{
    CommonTracePointMeta, ExtTracePoint, RawTraceEventFunc, TraceCallbackType, TraceDefaultFunc,
    TraceEntry, TraceEventFunc, TracePoint,
};
pub use tp_lexer;
use tp_lexer::compile_with_schema;
pub use trace_pipe::{
    TraceCmdLineCache, TraceCmdLineCacheSnapshot, TraceEntryParser, TracePipeOps, TracePipeRaw,
    TracePipeRecord, TracePipeSnapshot,
};

/// KernelTraceOps trait provides kernel-level operations for tracing.
pub trait KernelTraceOps: Send + Sync + 'static {
    /// Get the current process ID.
    fn current_pid() -> u32;
    /// Push a raw record to the trace pipe.
    fn trace_pipe_push_raw_record(buf: &[u8]);
    /// Cache the process name for a given PID.
    fn trace_cmdline_push(pid: u32);
    /// Access runtime state for a tracepoint ID.
    ///
    /// Implementations may hold a read-side lock while executing `f`. The
    /// tracing fast path may call user callbacks from inside this closure, so
    /// callbacks must not call APIs that need `write_tracepoint_state`, such as
    /// callback registration, callback unregistration, or filter updates.
    ///
    /// If the read-side implementation is non-reentrant, callbacks must also
    /// avoid recursively triggering tracepoints backed by the same state
    /// registry. Violating these restrictions may deadlock.
    ///
    /// Implementations based on RCU, snapshots, or another non-blocking
    /// read-side mechanism may relax these restrictions.
    fn read_tracepoint_state<R>(id: u32, f: impl FnOnce(&ExtTracePoint<Self>) -> R) -> R;
    /// Mutably access runtime state for a tracepoint ID.
    ///
    /// This is a management-path API. If `read_tracepoint_state` can execute
    /// callbacks while holding a read-side lock, this method must not be called
    /// from those callbacks. The update closure must not recursively invoke
    /// `write_tracepoint_state` for the same state unless the implementation
    /// explicitly provides a reentrant writer.
    fn write_tracepoint_state<R>(id: u32, f: impl FnOnce(&mut ExtTracePoint<Self>) -> R) -> R;
}

/// TracePointMap is a mapping from tracepoint IDs to TracePoint references.
#[derive(Debug)]
pub struct TracePointMap<K: KernelTraceOps>(BTreeMap<u32, &'static TracePoint<K>>);

impl<K: KernelTraceOps> TracePointMap<K> {
    /// Create a new TracePointMap
    pub const fn new() -> Self {
        Self(BTreeMap::new())
    }
}

impl<K: KernelTraceOps> Deref for TracePointMap<K> {
    type Target = BTreeMap<u32, &'static TracePoint<K>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K: KernelTraceOps> DerefMut for TracePointMap<K> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// TracePointFormatFile provides a way to get the format of the tracepoint.
#[derive(Debug, Clone)]
pub struct TracePointFormatFile<K: KernelTraceOps> {
    tracepoint: &'static TracePoint<K>,
}

impl<K: KernelTraceOps> TracePointFormatFile<K> {
    /// Create a new TracePointFormatFile for the given tracepoint.
    pub fn new(tracepoint: &'static TracePoint<K>) -> Self {
        Self { tracepoint }
    }

    /// Read the tracepoint format
    ///
    /// Returns the format string of the tracepoint.
    pub fn read(&self) -> String {
        self.tracepoint.print_fmt()
    }
}

/// TracePointEnableFile provides a way to enable or disable the default trace pipe callback.
#[derive(Debug, Clone)]
pub struct TracePointEnableFile;

impl TracePointEnableFile {
    /// Create a new TracePointEnableFile
    pub fn new() -> Self {
        Self {}
    }

    /// Read whether callback dispatch is enabled.
    ///
    /// Returns true if the tracepoint is enabled, false otherwise.
    pub fn read<K: KernelTraceOps>(&self, tracepoint: &'static TracePoint<K>) -> &'static str {
        if tracepoint.key_is_enabled() {
            "1\n"
        } else {
            "0\n"
        }
    }
    /// Register or unregister the default trace pipe callback.
    ///
    /// The runtime-state publisher is responsible for making the resulting
    /// callback gate agree with the newly published callback set.
    pub fn write<K: KernelTraceOps>(&self, ext_tracepoint: &mut ExtTracePoint<K>, enable: char) {
        match enable {
            '1' => {
                let default_callback = ext_tracepoint.default_callback();
                ext_tracepoint.register(TraceCallbackType::Default(default_callback));
            }
            '0' => {
                let default_callback = ext_tracepoint.default_callback();
                ext_tracepoint.unregister(TraceCallbackType::Default(default_callback));
            }
            _ => {
                log::warn!("Invalid value for tracepoint enable: {enable}");
            }
        }
    }
}

/// TracePointIdFile provides a way to read the tracepoint ID.
#[derive(Debug, Clone)]
pub struct TracePointIdFile<K: KernelTraceOps> {
    tracepoint: &'static TracePoint<K>,
}

impl<K: KernelTraceOps> TracePointIdFile<K> {
    /// Create a new TracePointIdFile with the given tracepoint.
    pub fn new(tracepoint: &'static TracePoint<K>) -> Self {
        Self { tracepoint }
    }

    /// Read the tracepoint ID
    ///
    /// Returns the ID of the tracepoint.
    pub fn read(&self) -> String {
        format!("{}\n", self.tracepoint.id())
    }
}

/// TraceFilterFile provides a way to set filters on the tracepoint.
#[derive(Debug)]
pub struct TraceFilterFile {
    filter_expr: Option<String>,
    pre_error: Option<String>,
}

impl TraceFilterFile {
    /// Create a new TraceFilterFile with no filter expression and no pre-error.
    pub fn new() -> Self {
        Self {
            filter_expr: None,
            pre_error: None,
        }
    }

    /// Read the tracepoint filter.
    ///
    /// Returns the current filter expression or an error message if there was a pre-error.
    pub fn read(&self) -> String {
        if let Some(err) = self.pre_error.as_ref() {
            return err.clone();
        }
        if let Some(filter) = self.filter_expr.as_ref() {
            filter.clone()
        } else {
            "none\n".to_string()
        }
    }

    /// Write a new filter expression to the tracepoint.
    pub fn write<K: KernelTraceOps>(
        &mut self,
        ext_tracepoint: &mut ExtTracePoint<K>,
        filter: &str,
    ) -> Result<(), &'static str> {
        if filter.as_bytes()[0] == b'0' {
            // clear the filter and pre-error
            self.filter_expr = None;
            self.pre_error = None;
            ext_tracepoint.set_compiled_expr(None);
            Ok(())
        } else {
            let schema = ext_tracepoint.schema();
            let res = compile_with_schema(filter, *schema);
            match res {
                Ok(compiled_expr) => {
                    self.filter_expr = Some(filter.to_string());
                    self.pre_error = None;
                    ext_tracepoint.set_compiled_expr(Some(compiled_expr));
                    Ok(())
                }
                Err(mut e) => {
                    e.message.push('\n');
                    self.pre_error = Some(e.message);
                    self.filter_expr = None;
                    ext_tracepoint.set_compiled_expr(None);
                    Err("compile error")
                }
            }
        }
    }
}

unsafe extern "C" {
    fn __start_tracepoint();
    fn __stop_tracepoint();
}

/// Initialize the tracing events
///
/// The K type parameter is the kernel trace operations type used for performing kernel-level operations.
///
/// Returns the static tracepoint map and runtime tracepoint states.
///
/// The returned [`TracePointMap`] maps tracepoint IDs to static tracepoint
/// metadata. The returned [`ExtTracePoint`] values contain runtime callback and
/// filter state. The caller must install these runtime states into the
/// [`KernelTraceOps::read_tracepoint_state`] and
/// [`KernelTraceOps::write_tracepoint_state`] backing registry before enabling
/// or triggering tracepoints.
pub fn global_init_events<K: KernelTraceOps>()
-> Result<(TracePointMap<K>, Vec<ExtTracePoint<K>>), &'static str> {
    static TRACE_POINT_ID: AtomicU32 = AtomicU32::new(0);
    let tracepoint_data_start = __start_tracepoint as *mut CommonTracePointMeta<K>;
    let tracepoint_data_end = __stop_tracepoint as *mut CommonTracePointMeta<K>;
    log::info!(
        "tracepoint_data_start: {:#x}, tracepoint_data_end: {:#x}",
        tracepoint_data_start as usize,
        tracepoint_data_end as usize
    );
    let tracepoint_data_len = (tracepoint_data_end as usize - tracepoint_data_start as usize)
        / size_of::<CommonTracePointMeta<K>>();
    let tracepoint_data =
        unsafe { core::slice::from_raw_parts_mut(tracepoint_data_start, tracepoint_data_len) };
    tracepoint_data.sort_by(|a, b| {
        a.trace_point
            .name()
            .cmp(b.trace_point.name())
            .then(a.trace_point.system().cmp(b.trace_point.system()))
    });
    log::info!("tracepoint_data_len: {tracepoint_data_len}");

    let mut tp_map = TracePointMap::new();
    let mut ext_tps = Vec::with_capacity(tracepoint_data_len);

    for tracepoint_meta in tracepoint_data.iter() {
        let tracepoint = tracepoint_meta.trace_point;
        let id = TRACE_POINT_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        tracepoint.set_id(id);

        let default_callback = Arc::new(TraceDefaultFunc {
            func: tracepoint_meta.print_func,
            data: Box::new(()),
        });

        let ext_tracepoint = ExtTracePoint::new(tracepoint, default_callback);

        log::info!(
            "tracepoint registered: {}:{}",
            tracepoint.system(),
            tracepoint.name(),
        );

        tp_map.insert(id, tracepoint);
        ext_tps.push(ext_tracepoint);
    }

    Ok((tp_map, ext_tps))
}
