use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};
use core::{
    any::{Any, TypeId},
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use tp_lexer::{Compiled, FieldClassifier, Schema};

use crate::{KernelTraceOps, TraceParseError};

type TraceEventCallback = dyn Fn(&mut [u8], &(dyn Any + Send + Sync)) + Send + Sync;
type RawTraceEventCallback = dyn Fn(&[u64], &(dyn Any + Send + Sync)) + Send + Sync;

/// A trace entry structure that holds metadata about a trace event.
#[derive(Clone, Copy, Debug)]
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

/// A field type that can be copied from an arbitrary initialized byte buffer.
///
/// # Safety
///
/// Every bit pattern must be a valid value of the implementing type, the type
/// must not own resources that require drop, and its object representation
/// must not contain padding bytes. Generated event encoders copy each field's
/// complete object representation into a zero-initialized record, while event
/// formatters perform an unaligned copy from that record.
pub unsafe trait TraceField: FieldClassifier + Copy {}

macro_rules! impl_trace_field {
    ($($type:ty),+ $(,)?) => {
        $(
            // SAFETY: integer primitives accept every bit pattern, are Copy,
            // and contain no padding bytes.
            unsafe impl TraceField for $type {}
        )+
    };
}

impl_trace_field!(i8, i16, i32, i64, i128, isize);
impl_trace_field!(u8, u16, u32, u64, u128, usize);

// SAFETY: an array is byte-valid, Copy, and padding-free when each element has
// those properties; arrays do not add padding between elements.
unsafe impl<T: TraceField, const LEN: usize> TraceField for [T; LEN] {}

/// Linker-collected metadata for one tracepoint.
#[doc(hidden)]
#[repr(C)]
pub struct CommonTracePointMeta {
    kernel_type_id: fn() -> TypeId,
    trace_point: *const (),
    print_func: fn(),
}

// SAFETY: metadata is immutable after link. `trace_point` is produced only
// from a shared reference to a `TracePoint<K>`, which is Sync by the
// `KernelTraceOps: Send + Sync` bound, and function pointers are Sync.
unsafe impl Sync for CommonTracePointMeta {}

fn kernel_type_id<K: KernelTraceOps>() -> TypeId {
    TypeId::of::<K>()
}

impl CommonTracePointMeta {
    /// Constructs linker metadata from a generated tracepoint callback.
    ///
    /// # Safety
    ///
    /// `print_func` must be the type-erased form of the default callback
    /// generated for `trace_point`. It may only be restored to that exact
    /// signature by the macro that defined the tracepoint.
    #[doc(hidden)]
    pub const unsafe fn new<K: KernelTraceOps>(
        trace_point: &'static TracePoint<K>,
        print_func: fn(),
    ) -> Self {
        Self {
            kernel_type_id: kernel_type_id::<K>,
            trace_point: core::ptr::from_ref(trace_point).cast(),
            print_func,
        }
    }

    pub(crate) fn belongs_to<K: KernelTraceOps>(&self) -> bool {
        (self.kernel_type_id)() == TypeId::of::<K>()
    }

    /// Restores the tracepoint type recorded by [`Self::belongs_to`].
    ///
    /// # Safety
    ///
    /// The caller must first verify that `self.belongs_to::<K>()` is true.
    pub(crate) unsafe fn trace_point<K: KernelTraceOps>(&self) -> &'static TracePoint<K> {
        // SAFETY: the caller verified the type tag installed from the same
        // `TracePoint<K>` reference by `new`.
        unsafe { &*self.trace_point.cast::<TracePoint<K>>() }
    }

    pub(crate) const fn print_func(&self) -> fn() {
        self.print_func
    }
}

/// A structure representing a registered tracepoint callback function.
pub struct TraceEventFunc {
    /// The callback function to be called when the tracepoint is hit.
    /// The function receives exclusive access to one generated event record
    /// and a reference to its associated data.
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
    pub fn call(&self, entry: &mut [u8]) {
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
    func: fn(),
    data: Box<dyn Any + Send + Sync>,
}

impl TraceDefaultFunc {
    /// Constructs a callback from a type-erased generated function.
    ///
    /// # Safety
    ///
    /// `func` must be restored only to the exact callback signature from which
    /// it was erased. The generated registration and dispatch functions uphold
    /// this pairing.
    #[doc(hidden)]
    pub unsafe fn from_erased(func: fn(), data: Box<dyn Any + Send + Sync>) -> Self {
        Self { func, data }
    }

    /// Returns the erased callback pointer for generated dispatch code.
    #[doc(hidden)]
    pub const fn erased_func(&self) -> fn() {
        self.func
    }

    /// Returns the payload associated with this callback.
    #[doc(hidden)]
    pub fn data(&self) -> &(dyn Any + Send + Sync) {
        self.data.as_ref()
    }
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
    trace_entry_fmt_func: fn(&[u8]) -> Result<String, TraceParseError>,
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
        fmt_func: fn(&[u8]) -> Result<String, TraceParseError>,
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
    pub(crate) fn fmt_func(&self) -> fn(&[u8]) -> Result<String, TraceParseError> {
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
    #![allow(dead_code)]

    extern crate std;

    use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

    use tp_lexer::{FieldClassifier, Schema};

    use super::{ExtTracePoint, TraceCallbackType, TraceDefaultFunc, TraceEventFunc, TracePoint};
    use crate::{
        KernelTraceOps, TraceEntry, TraceEntryParser, TraceParseError, TracePipeRecord,
        TracePointMap,
    };

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

    struct OtherKernel;

    impl KernelTraceOps for OtherKernel {
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

    struct PaddingKernel;

    impl KernelTraceOps for PaddingKernel {
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

    crate::define_event_trace!(
        test_kernel_event,
        TP_kops(TestKernel),
        TP_system(ax_tracepoint_test),
        TP_PROTO(value: u32),
        TP_STRUCT__entry { value: u32 },
        TP_fast_assign { value: value },
        TP_ident(entry),
        TP_printk(format_args!("value={}", entry.value))
    );

    crate::define_event_trace!(
        other_kernel_event,
        TP_kops(OtherKernel),
        TP_system(ax_tracepoint_test),
        TP_PROTO(value: u32),
        TP_STRUCT__entry { value: u32 },
        TP_fast_assign { value: value },
        TP_ident(entry),
        TP_printk(format_args!("value={}", entry.value))
    );

    crate::define_event_trace!(
        padded_kernel_event,
        TP_kops(PaddingKernel),
        TP_system(ax_tracepoint_test),
        TP_PROTO(sequence: u64, state: u32),
        TP_STRUCT__entry {
            sequence: u64,
            state: u32,
        },
        TP_fast_assign {
            sequence: sequence,
            state: state,
        },
        TP_ident(entry),
        TP_printk(format_args!(
            "sequence={} state={}",
            entry.sequence, entry.state
        ))
    );

    fn format_entry(_: &[u8]) -> Result<String, TraceParseError> {
        Ok(String::new())
    }

    fn print_format() -> String {
        String::new()
    }

    fn default_callback() {}

    static TEST_FIELDS: &[(&str, tp_lexer::FieldType, usize, usize)] =
        &[("value", u32::FIELD_TYPE, 0, size_of::<u32>())];

    static TEST_POINT: TracePoint<TestKernel> = TracePoint::new(
        "callback_gate",
        "ax_tracepoint_test",
        format_entry,
        print_format,
        Schema::new(TEST_FIELDS),
    );

    #[test]
    fn cooked_event_callback_receives_an_exclusive_record() {
        let callback = TraceEventFunc::new(
            Box::new(|entry, _data| {
                entry[0] = 0xa5;
            }),
            Box::new(()),
        );
        let mut record = [0_u8; 1];

        callback.call(&mut record);

        assert_eq!(record, [0xa5]);
    }

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

    #[test]
    fn linker_discovery_filters_metadata_by_kernel_ops_type() {
        let (test_points, _) = crate::global_init_events::<TestKernel>().unwrap();
        let (other_points, _) = crate::global_init_events::<OtherKernel>().unwrap();

        assert_eq!(test_points.len(), 1);
        assert_eq!(other_points.len(), 1);
        assert_eq!(
            test_points.values().next().unwrap().name(),
            "test_kernel_event"
        );
        assert_eq!(
            other_points.values().next().unwrap().name(),
            "other_kernel_event"
        );
    }

    #[test]
    fn empty_filter_is_rejected_without_panicking() {
        let mut state = ExtTracePoint::new(
            &TEST_POINT,
            Arc::new(TraceDefaultFunc {
                func: default_callback,
                data: Box::new(()),
            }),
        );
        let mut filter = crate::TraceFilterFile::new();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            filter.write(&mut state, "")
        }));

        assert!(result.is_ok(), "an empty filter must not panic");
        assert_eq!(
            result.unwrap(),
            Err(crate::TraceFilterError::EmptyExpression)
        );
    }

    #[test]
    fn invalid_filter_preserves_the_previous_compiled_expression() {
        let mut state = ExtTracePoint::new(
            &TEST_POINT,
            Arc::new(TraceDefaultFunc {
                func: default_callback,
                data: Box::new(()),
            }),
        );
        let mut filter = crate::TraceFilterFile::new();

        assert_eq!(filter.write(&mut state, "value == 1"), Ok(()));
        assert!(state.get_compiled_expr().is_some());
        assert_eq!(
            filter.write(&mut state, "value ??? 1"),
            Err(crate::TraceFilterError::CompileExpression)
        );
        assert!(
            state.get_compiled_expr().is_some(),
            "a rejected update must not clear the active filter"
        );
    }

    #[test]
    fn unknown_tracepoint_record_is_rejected_without_panicking() {
        let header = TraceEntry {
            common_type: 17,
            common_flags: 0,
            common_preempt_count: 0,
            common_pid: 1,
        };
        // SAFETY: `header` is fully initialized and is copied only for the
        // duration of this statement.
        let event = unsafe {
            core::slice::from_raw_parts(
                core::ptr::from_ref(&header).cast::<u8>(),
                size_of::<TraceEntry>(),
            )
        };
        let record = TracePipeRecord::new(0, 0, Vec::from(event));
        let map = TracePointMap::<TestKernel>::new();
        let cmdlines = crate::TraceCmdLineCache::new(core::num::NonZero::new(1).unwrap());

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            TraceEntryParser::parse(&map, &cmdlines, &record)
        }));
        assert!(result.is_ok(), "an unknown tracepoint ID must not panic");
        assert_eq!(
            result.unwrap(),
            Err(TraceParseError::UnknownTracepoint { id: 17 })
        );
    }

    #[test]
    fn short_tracepoint_record_is_rejected_before_header_decode() {
        let record = TracePipeRecord::new(0, 0, Vec::new());
        let map = TracePointMap::<TestKernel>::new();
        let cmdlines = crate::TraceCmdLineCache::new(core::num::NonZero::new(1).unwrap());

        assert_eq!(
            TraceEntryParser::parse(&map, &cmdlines, &record),
            Err(TraceParseError::RecordTooShort {
                expected: size_of::<TraceEntry>(),
                actual: 0,
            })
        );
    }

    #[test]
    fn generated_record_zeroes_c_layout_padding() {
        let record = encode_padded_kernel_event_record(
            TraceEntry {
                common_type: 7,
                common_flags: 0,
                common_preempt_count: 0,
                common_pid: 11,
            },
            0x1122_3344_5566_7788,
            0xaabb_ccdd,
        );
        let padding_start =
            core::mem::offset_of!(__padded_kernel_event_full_entry, entry.state) + size_of::<u32>();
        let padding_end = size_of::<__padded_kernel_event_full_entry>();

        assert!(
            padding_start < padding_end,
            "the regression layout must contain tail padding"
        );
        assert!(
            record.as_bytes()[padding_start..padding_end]
                .iter()
                .all(|byte| *byte == 0),
            "all bytes exposed to trace callbacks must be initialized"
        );
    }
}
