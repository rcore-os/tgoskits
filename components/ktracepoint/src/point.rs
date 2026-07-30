use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};
use core::{
    any::Any,
    marker::PhantomData,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use tp_lexer::{Compiled, Schema};

use crate::KernelTraceOps;

type TraceEventCallback = dyn Fn(&[u8], &(dyn Any + Send + Sync)) + Send + Sync;
type RawTraceEventCallback = dyn Fn(&[u64], &(dyn Any + Send + Sync)) + Send + Sync;

/// A trace entry structure that holds metadata about a trace event.
#[derive(Debug)]
#[repr(C)]
pub struct TraceEntry {
    /// The type of the trace event, typically the tracepoint ID.
    pub common_type: u16,
    /// Flags associated with the trace event.
    pub common_flags: u8,
    /// The preemption count at the time of the event.
    pub common_preempt_count: u8,
    /// The PID of the process that generated the event.
    pub common_pid: i32,
}

impl TraceEntry {
    /// Returns a formatted string representing the latency and preemption state.
    pub fn trace_print_lat_fmt(&self) -> String {
        // todo!("Implement IRQs off logic");
        let irqs_off = '.';
        let resched = '.';
        let hardsoft_irq = '.';
        let mut preempt_low = '.';
        if self.common_preempt_count & 0xf != 0 {
            preempt_low = ((b'0') + (self.common_preempt_count & 0xf)) as char;
        }
        let mut preempt_high = '.';
        if self.common_preempt_count >> 4 != 0 {
            preempt_high = ((b'0') + (self.common_preempt_count >> 4)) as char;
        }
        format!("{irqs_off}{resched}{hardsoft_irq}{preempt_low}{preempt_high}")
    }
}

/// CommonTracePointMeta holds metadata for a common tracepoint.
#[derive(Debug)]
#[repr(C)]
pub struct CommonTracePointMeta<K: KernelTraceOps> {
    /// A reference to the tracepoint.
    pub trace_point: &'static TracePoint<K>,
    /// The print function for the tracepoint.
    pub print_func: fn(),
}

/// A structure representing a registered tracepoint callback function.
pub struct TraceEventFunc {
    /// The callback function to be called when the tracepoint is hit.
    /// The function takes a byte slice representing the raw trace entry data and a reference to any associated data.
    func: Box<TraceEventCallback>,
    /// The data associated with the callback function.
    data: Box<dyn Any + Send + Sync>,
    perf_enable: AtomicBool,
}

impl TraceEventFunc {
    /// Creates a new TraceEventFunc instance.
    pub fn new(func: Box<TraceEventCallback>, data: Box<dyn Any + Send + Sync>) -> Self {
        Self {
            func,
            data,
            perf_enable: AtomicBool::new(false),
        }
    }

    /// Calls the callback function with the provided trace entry data.
    pub fn call(&self, entry: &[u8]) {
        (self.func)(entry, &self.data);
    }

    /// Enable or disable perf event for this callback function.
    pub fn set_perf_enable(&self, enable: bool) {
        self.perf_enable.store(enable, Ordering::Relaxed);
    }

    /// Returns true if perf event is enabled for this callback function, false otherwise.
    pub fn perf_enabled(&self) -> bool {
        self.perf_enable.load(Ordering::Relaxed)
    }
}

/// A structure representing a registered raw tracepoint callback function.
pub struct RawTraceEventFunc {
    /// The callback function to be called when the tracepoint is hit, with raw arguments.
    /// The function takes a slice of u64 representing the raw arguments and a reference to any associated data.
    func: Box<RawTraceEventCallback>,
    /// The data associated with the callback function.
    data: Box<dyn Any + Send + Sync>,
}

impl RawTraceEventFunc {
    /// Creates a new RawTraceEventFunc instance.
    pub fn new(func: Box<RawTraceEventCallback>, data: Box<dyn Any + Send + Sync>) -> Self {
        Self { func, data }
    }
    /// Calls the callback function with the provided raw arguments.
    pub fn call(&self, args: &[u64]) {
        (self.func)(args, &self.data);
    }
}

/// A structure representing a registered tracepoint callback function.
#[derive(Debug)]
pub struct TraceDefaultFunc {
    /// The static function pointer for the format function of the tracepoint.
    pub func: fn(),
    /// The data associated with the callback function.
    pub data: Box<dyn Any + Send + Sync>,
}

/// An enum representing the different types of tracepoint callback functions.
#[derive(Clone)]
pub enum TraceCallbackType {
    /// The default callback function for the tracepoint, typically used for the default print functionality.
    Default(Arc<TraceDefaultFunc>),
    /// A custom event callback function for the tracepoint, used for custom event handling.
    Event(Arc<TraceEventFunc>),
    /// A custom raw event callback function for the tracepoint, used for handling raw tracepoint events with raw arguments.
    RawEvent(Arc<RawTraceEventFunc>),
}

impl PartialEq for TraceCallbackType {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TraceCallbackType::Default(func1), TraceCallbackType::Default(func2)) => {
                Arc::ptr_eq(func1, func2)
            }
            (TraceCallbackType::Event(func1), TraceCallbackType::Event(func2)) => {
                Arc::ptr_eq(func1, func2)
            }
            (TraceCallbackType::RawEvent(func1), TraceCallbackType::RawEvent(func2)) => {
                Arc::ptr_eq(func1, func2)
            }
            _ => false,
        }
    }
}

/// The TracePoint structure represents a tracepoint in the system.
pub struct TracePoint<K: KernelTraceOps> {
    name: &'static str,
    system: &'static str,
    callbacks_enabled: AtomicBool,
    id: AtomicU32,
    trace_entry_fmt_func: fn(&[u8]) -> String,
    trace_print_func: fn() -> String,
    schema: Schema,
    flags: u8,
    kernel: PhantomData<fn() -> K>,
}

impl<K: KernelTraceOps> core::fmt::Debug for TracePoint<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TracePoint")
            .field("name", &self.name)
            .field("system", &self.system)
            .field("id", &self.id())
            .field("flags", &self.flags)
            .finish()
    }
}

/// An extended tracepoint structure that includes additional callback management and compiled expression handling.
pub struct ExtTracePoint<K: KernelTraceOps> {
    tracepoint: &'static TracePoint<K>,
    callbacks: Vec<TraceCallbackType>,
    compiled_expr: Option<Compiled>,
    default_callback: Arc<TraceDefaultFunc>,
}

impl<K: KernelTraceOps> Clone for ExtTracePoint<K> {
    fn clone(&self) -> Self {
        Self {
            tracepoint: self.tracepoint,
            callbacks: self.callbacks.clone(),
            compiled_expr: self.compiled_expr.clone(),
            default_callback: Arc::clone(&self.default_callback),
        }
    }
}

impl<K: KernelTraceOps> core::fmt::Debug for ExtTracePoint<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ExtTracePoint")
            .field("tracepoint", &self.tracepoint)
            .finish()
    }
}

impl<K: KernelTraceOps> Deref for ExtTracePoint<K> {
    type Target = TracePoint<K>;

    fn deref(&self) -> &Self::Target {
        self.tracepoint
    }
}

impl<K: KernelTraceOps> ExtTracePoint<K> {
    /// Creates a new ExtTracePoint instance.
    pub const fn new(
        tracepoint: &'static TracePoint<K>,
        default_callback: Arc<TraceDefaultFunc>,
    ) -> Self {
        Self {
            tracepoint,
            callbacks: Vec::new(),
            default_callback,
            compiled_expr: None,
        }
    }

    /// Returns a reference to the default callback function for the tracepoint.
    pub fn default_callback(&self) -> Arc<TraceDefaultFunc> {
        self.default_callback.clone()
    }

    /// Returns a reference to the underlying TracePoint.
    pub const fn trace_point(&self) -> &'static TracePoint<K> {
        self.tracepoint
    }

    /// Sets the compiled expression for the tracepoint.
    pub fn set_compiled_expr(&mut self, compiled: Option<Compiled>) {
        self.compiled_expr = compiled;
    }

    /// Returns the compiled expression for the tracepoint.
    pub fn get_compiled_expr(&self) -> Option<&Compiled> {
        self.compiled_expr.as_ref()
    }

    /// Register a callback in this runtime-state value.
    ///
    /// This does not make the tracepoint globally visible. The owner must
    /// publish the complete state first and then update the callback gate.
    pub fn register(&mut self, callback: TraceCallbackType) {
        if !self.callbacks.iter().any(|f| f == &callback) {
            self.callbacks.push(callback);
        }
    }

    /// Unregister a callback from this runtime-state value.
    ///
    /// This does not change the global callback gate. The owner must order the
    /// gate transition with publication of the complete replacement state.
    pub fn unregister(&mut self, callback: TraceCallbackType) {
        self.callbacks.retain(|f| f != &callback);
    }

    /// Returns whether this runtime state has at least one callback.
    pub fn has_callbacks(&self) -> bool {
        !self.callbacks.is_empty()
    }

    /// Iterate over all registered callback functions
    pub fn callback_list(&self) -> impl Iterator<Item = &TraceCallbackType> {
        self.callbacks.iter()
    }
}

impl<K: KernelTraceOps> TracePoint<K> {
    /// Creates a new TracePoint instance.
    pub const fn new(
        name: &'static str,
        system: &'static str,
        fmt_func: fn(&[u8]) -> String,
        trace_print_func: fn() -> String,
        schema: Schema,
    ) -> Self {
        Self {
            name,
            system,
            callbacks_enabled: AtomicBool::new(false),
            id: AtomicU32::new(0),
            flags: 0,
            trace_entry_fmt_func: fmt_func,
            trace_print_func,
            schema,
            kernel: PhantomData,
        }
    }

    /// Returns the schema of the tracepoint.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Returns the name of the tracepoint.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the system of the tracepoint.
    pub fn system(&self) -> &'static str {
        self.system
    }

    /// Sets the ID of the tracepoint.
    pub(crate) fn set_id(&self, id: u32) {
        self.id.store(id, Ordering::Relaxed);
    }

    /// Returns the ID of the tracepoint.
    pub fn id(&self) -> u32 {
        self.id.load(Ordering::Relaxed)
    }

    /// Returns the flags of the tracepoint.
    pub fn flags(&self) -> u8 {
        self.flags
    }

    /// Returns the format function for the tracepoint.
    pub(crate) fn fmt_func(&self) -> fn(&[u8]) -> String {
        self.trace_entry_fmt_func
    }

    /// Returns a string representation of the format function for the tracepoint.
    ///
    /// You can use `cat /sys/kernel/debug/tracing/events/syscalls/sys_enter_openat/format` in linux
    /// to see the format of the tracepoint.
    pub fn print_fmt(&self) -> String {
        let post_str = (self.trace_print_func)();
        format!("name: {}\nID: {}\n{}\n", self.name(), self.id(), post_str)
    }

    /// Publishes whether the callback fast path should enter runtime state.
    ///
    /// Enabling must happen after publishing a non-empty callback state.
    /// Disabling must happen before retiring the last non-empty state. A
    /// Release store pairs with the fast path's Acquire load so a reader that
    /// observes `true` can also observe the published callback snapshot.
    pub fn set_callback_gate(&self, enabled: bool) {
        self.callbacks_enabled.store(enabled, Ordering::Release);
    }

    /// Returns true if the tracepoint is enabled, false otherwise.
    pub fn key_is_enabled(&self) -> bool {
        self.callbacks_enabled.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, string::String, sync::Arc};

    use tp_lexer::Schema;

    use super::{ExtTracePoint, TraceCallbackType, TraceDefaultFunc, TracePoint};
    use crate::KernelTraceOps;

    struct TestKernel;

    impl KernelTraceOps for TestKernel {
        fn current_pid() -> u32 {
            0
        }

        fn trace_pipe_push_raw_record(_: &[u8]) {}

        fn trace_cmdline_push(_: u32) {}

        fn read_tracepoint_state<R>(_: u32, _: impl FnOnce(&ExtTracePoint<Self>) -> R) -> R {
            panic!("test has no tracepoint registry")
        }

        fn write_tracepoint_state<R>(_: u32, _: impl FnOnce(&mut ExtTracePoint<Self>) -> R) -> R {
            panic!("test has no tracepoint registry")
        }
    }

    fn format_entry(_: &[u8]) -> String {
        String::new()
    }

    fn print_format() -> String {
        String::new()
    }

    fn default_callback() {}

    static TEST_POINT: TracePoint<TestKernel> = TracePoint::new(
        "callback_gate",
        "ktracepoint_test",
        format_entry,
        print_format,
        Schema::new(&[]),
    );

    #[test]
    fn callback_state_changes_are_side_effect_free_until_the_owner_publishes_the_gate() {
        TEST_POINT.set_callback_gate(false);
        let mut state = ExtTracePoint::new(
            &TEST_POINT,
            Arc::new(TraceDefaultFunc {
                func: default_callback,
                data: Box::new(()),
            }),
        );
        let callback = TraceCallbackType::Default(state.default_callback());

        state.register(callback.clone());
        assert!(state.has_callbacks());
        assert!(!TEST_POINT.key_is_enabled());

        let published = state.clone();
        published.trace_point().set_callback_gate(true);
        assert!(state.trace_point().key_is_enabled());

        state.unregister(callback);
        assert!(!state.has_callbacks());
        assert!(TEST_POINT.key_is_enabled());

        state.trace_point().set_callback_gate(false);
        assert!(!published.trace_point().key_is_enabled());
    }
}
