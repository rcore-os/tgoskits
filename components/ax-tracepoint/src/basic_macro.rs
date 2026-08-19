/// Define a tracepoint with the given parameters.
///
/// This macro generates a tracepoint with the specified name, arguments, entry structure, assignment logic, identifier, and print format.
/// # Parameters
/// - `name`: The name of the tracepoint.
/// - `TP_kops`: The kernel trace operations type. `[crate::KernelTraceOps]` is expected to be implemented for this type.
/// - `TP_system`: The subsystem or system to which the tracepoint belongs.
/// - `TP_PROTO`: The prototype of the tracepoint function.
/// - `TP_STRUCT__entry`: The structure of the tracepoint entry.
///   Every field must implement [`crate::TraceField`]. The macro gives the
///   generated entry C layout and performs unaligned copies when formatting.
/// - `TP_fast_assign`: The assignment logic for the tracepoint entry.
/// - `TP_ident`: The identifier for the tracepoint entry.
/// - `TP_printk`: The print format for the tracepoint.
///
/// # Example
/// ```rust ignore
/// use crate::KernelTraceOps;
/// define_event_trace!(
///     TEST2,
///     TP_kops(Kops),
///     TP_system(tracepoint_test),
///     TP_PROTO(a: u32, b: u32),
///     TP_STRUCT__entry{
///           a: u32,
///           b: u32,
///     },
///     TP_fast_assign{
///           a:a,
///           b:{
///             // do something with b
///             b
///           }
///     },
///     TP_ident(__entry),
///     TP_printk({
///           // do something with __entry
///           format!("Hello from tracepoint! a={}, b={}", __entry.a, __entry.b)
///     })
/// );
/// ```
#[macro_export]
macro_rules! define_event_trace{
    (
        $name:ident,
        TP_kops($kops:path),
        TP_system($system:ident),
        TP_PROTO($($arg:ident:$arg_type:ty),+ $(,)?),
        TP_STRUCT__entry{$($entry:ident:$entry_type:ty),+ $(,)?},
        TP_fast_assign{$($assign:ident:$value:expr),+ $(,)?},
        TP_ident($tp_ident:ident),
        TP_printk($fmt_expr: expr)
    ) => {
        $crate::paste::paste!{
            #[allow(non_upper_case_globals)]
            #[used]
            static [<__ $name>]: $crate::TracePoint<$kops> = {
                #[repr(C)]
                #[derive(Clone, Copy)]
                struct Entry {
                    $($entry: $entry_type,)*
                }
                #[repr(C)]
                struct FullEntry {
                    common: $crate::TraceEntry,
                    entry: Entry,
                }
                use $crate::tp_lexer::{schema,FieldClassifier};
                const fn assert_trace_field<T: $crate::TraceField>() {}
                $(assert_trace_field::<$entry_type>();)*
                let schema = schema!(
                    "common_type" => (u16::FIELD_TYPE, 0, 2),
                    "common_flags" => (u8::FIELD_TYPE, 2, 1),
                    "common_preempt_count" => (u8::FIELD_TYPE, 3, 1),
                    "common_pid" => (i32::FIELD_TYPE, 4, 4),
                    $(
                        stringify!($entry) => (<$entry_type>::FIELD_TYPE, core::mem::offset_of!(FullEntry, entry.$entry), core::mem::size_of::<$entry_type>()),
                    )*
                );
                $crate::TracePoint::new(stringify!($name), stringify!($system),[<trace_fmt_ $name>], [<trace_fmt_show $name>], schema)
            };

            #[inline(always)]
            #[allow(non_snake_case)]
            pub fn [<trace_ $name>]( $($arg:$arg_type),* ){
                let default_handler = |ext_tp: &$crate::ExtTracePoint<$kops>, trace_default_func: &$crate::TraceDefaultFunc |{
                    let func = trace_default_func.erased_func();
                    let data = trace_default_func.data();
                    if func as *const () as usize
                        == [<trace_default_ $name>]::<$kops> as *const () as usize
                    {
                        let tp_compiled_expr = ext_tp.get_compiled_expr();
                        let func = [<trace_default_ $name>]::<$kops>;
                        func(tp_compiled_expr, data, $($arg),*);
                    }else {
                        // SAFETY: the only safe constructor for this callback
                        // is the generated `register_trace_*` function below,
                        // which erases this exact signature.
                        let func = unsafe{core::mem::transmute::<fn(),fn(& (dyn core::any::Any+Send+Sync), $($arg_type),*)>(func)};
                        func(data $(,$arg)*);
                    }
                };

                let event_handler = |event_func: &$crate::TraceEventFunc|{
                    if event_func.perf_enabled(){
                        #[repr(C)]
                        #[derive(Clone, Copy)]
                        struct Entry {
                            $($entry: $entry_type,)*
                        }
                        #[repr(C)]
                        struct FullEntry {
                            common: $crate::TraceEntry,
                            entry: Entry,
                        }

                        let entry = Entry {
                            $($assign: $value,)*
                        };
                        use $crate::KernelTraceOps;
                        let pid = $kops::current_pid();
                        let common = $crate::TraceEntry {
                            common_type: [<__ $name>].id() as _,
                            common_flags: [<__ $name>].flags(),
                            common_preempt_count: 0,
                            common_pid: pid as i32,
                        };

                        let full_entry = FullEntry {
                            common,
                            entry,
                        };

                        let event_buf = unsafe {
                            core::slice::from_raw_parts(
                                &full_entry as *const FullEntry as *const u8,
                                core::mem::size_of::<FullEntry>(),
                            )
                        };
                        event_func.call(event_buf);
                    }
                };

                let raw_event_handler = |raw_func: &$crate::RawTraceEventFunc|{
                    let args = [$($crate::ptr::AsU64::as_u64($arg)),*];
                    raw_func.call(&args);
                };

                use $crate::KernelTraceOps;
                if [<__ $name>].key_is_enabled() {
                    $kops::read_tracepoint_state([<__ $name>].id(), |ext_tp|{
                        for callback in ext_tp.callback_list() {
                            match callback {
                              $crate::TraceCallbackType::Default(default_func) => {
                                  default_handler(ext_tp, default_func);
                              },
                              $crate::TraceCallbackType::Event(event_func) => {
                                  event_handler(event_func);
                              },
                              $crate::TraceCallbackType::RawEvent(raw_func) => {
                                  raw_event_handler(raw_func);
                              }
                            }
                        }
                    });
                }
            }

            #[allow(non_snake_case)]
            pub fn [<register_trace_ $name>](func: fn(& (dyn core::any::Any+Send+Sync), $($arg_type),*), data: alloc::boxed::Box<dyn core::any::Any+Send+Sync>) -> $crate::TraceCallbackType {
                // SAFETY: dispatch restores this pointer to the same callback
                // signature generated from this macro invocation.
                let func = unsafe{core::mem::transmute::<fn(& (dyn core::any::Any+Send+Sync), $($arg_type),*), fn()>(func)};
                // SAFETY: `func` was erased from the exact signature paired
                // with this generated registration function.
                let callback = unsafe { $crate::TraceDefaultFunc::from_erased(func, data) };

                let callback_type = $crate::TraceCallbackType::Default(alloc::sync::Arc::new(callback));

                use $crate::KernelTraceOps;
                $kops::write_tracepoint_state([<__ $name>].id(), |ext_tp|{
                    ext_tp.register(callback_type.clone());
                });
                callback_type
            }

            #[allow(non_snake_case)]
            pub fn [<unregister_trace_ $name>](callback: $crate::TraceCallbackType){
                use $crate::KernelTraceOps;
                $kops::write_tracepoint_state([<__ $name>].id(), |ext_tp|{
                    ext_tp.unregister(callback);
                });
            }


            #[allow(non_upper_case_globals)]
            #[unsafe(link_section = ".tracepoint")]
            #[used]
            static [<__ $name _meta>]: $crate::CommonTracePointMeta = {
                // SAFETY: the erased pointer is stored next to the tracepoint
                // generated by the same macro invocation and restored only by
                // that invocation's dispatch path.
                let print_func = unsafe {
                    core::mem::transmute::<
                        fn(Option<&$crate::tp_lexer::Compiled>, &(dyn core::any::Any + Send + Sync), $($arg_type),*),
                        fn(),
                    >([<trace_default_ $name>]::<$kops>)
                };
                // SAFETY: `print_func` is the exact erased default callback
                // associated with this tracepoint.
                unsafe { $crate::CommonTracePointMeta::new(&[<__ $name>], print_func) }
            };

            #[allow(non_snake_case)]
            fn [<trace_default_ $name>]<F:$crate::KernelTraceOps>(tp_compiled_expr: Option<&$crate::tp_lexer::Compiled>, _data:& (dyn core::any::Any+Send+Sync), $($arg:$arg_type),* )
            {
                #[repr(C)]
                #[derive(Clone, Copy)]
                struct Entry {
                    $($entry: $entry_type,)*
                }
                #[repr(C)]
                struct FullEntry {
                    common: $crate::TraceEntry,
                    entry: Entry,
                }

                let entry = Entry {
                    $($assign: $value,)*
                };

                let pid = F::current_pid();
                let common = $crate::TraceEntry {
                    common_type: [<__ $name>].id() as _,
                    common_flags: [<__ $name>].flags(),
                    common_preempt_count: 0,
                    common_pid: pid as i32,
                };

                let full_entry = FullEntry {
                    common,
                    entry,
                };

                let event_buf = unsafe {
                    core::slice::from_raw_parts(
                        &full_entry as *const FullEntry as *const u8,
                        core::mem::size_of::<FullEntry>(),
                    )
                };

                if let Some(compiled_expr) = tp_compiled_expr {
                    use $crate::tp_lexer::BufContext;
                    let buf_ctx = BufContext::new(event_buf, &[<__ $name>].schema());
                    if !compiled_expr.evaluate(&buf_ctx) {
                        return;
                    }
                }

                F::trace_cmdline_push(pid);
                F::trace_pipe_push_raw_record(event_buf);
            }

            #[allow(non_snake_case)]
            pub fn [<trace_fmt_ $name>](buf: &[u8]) -> core::result::Result<alloc::string::String, $crate::TraceParseError> {
                #[repr(C)]
                #[derive(Clone, Copy)]
                struct Entry {
                    $($entry: $entry_type,)*
                }
                let expected = core::mem::size_of::<Entry>();
                if buf.len() < expected {
                    return Err($crate::TraceParseError::PayloadTooShort {
                        expected,
                        actual: buf.len(),
                    });
                }
                // SAFETY: `TraceField` requires every entry field to be Copy,
                // drop-free, and valid for every bit pattern. The length check
                // above covers the complete generated C-layout entry.
                let entry = unsafe { core::ptr::read_unaligned(buf.as_ptr().cast::<Entry>()) };
                let $tp_ident = &entry;
                let fmt = alloc::format!("{}", $fmt_expr);
                Ok(fmt)
            }

            #[allow(non_snake_case)]
            pub fn [<trace_fmt_show $name>]()-> alloc::string::String {
                let mut fmt = alloc::format!("format:
\tfield: u16 common_type; offset: 0; size: 2; signed: 0;
\tfield: u8 common_flags; offset: 2; size: 1; signed: 0;
\tfield: u8 common_preempt_count; offset: 3; size: 1; signed: 0;
\tfield: i32 common_pid; offset: 4; size: 4; signed: 1;

");
                fn is_signed<T>() -> bool {
                    match core::any::type_name::<T>() {
                        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => true,
                        _ => false,
                    }
                }


                #[repr(C)]
                struct Entry {
                    $($entry: $entry_type,)*
                }
                #[repr(C)]
                struct FullEntry {
                    common: $crate::TraceEntry,
                    entry: Entry,
                }

                $(
                    let offset = core::mem::offset_of!(FullEntry, entry.$entry);
                    fmt.push_str(&alloc::format!("\tfield: {} {} offset: {}; size: {}; signed: {};\n",
                        stringify!($entry_type), stringify!($entry), offset, core::mem::size_of::<$entry_type>(), if is_signed::<$entry_type>() { 1 } else { 0 }));
                )*
                fmt.push_str(&alloc::format!("\nprint fmt: \"{}\"", stringify!($fmt_expr)));
                fmt
            }
        }
    };
}
