extern crate alloc;
use std::{collections::BTreeMap, sync::Arc};

use ax_tracepoint::{
    KernelTraceOps, RawTraceEventFunc, TraceCallbackType, TraceCmdLineCache, TraceEntryParser,
    TraceEventFunc, TraceFilterFile, TracePointEnableFile, TracePointMap, global_init_events,
};

use crate::tracepoint_example::{EXT_TRACEPOINTS, Kops, TRACE_RAW_PIPE};

mod tracepoint_example {
    use std::{
        collections::BTreeMap,
        num::NonZero,
        ops::Deref,
        sync::{Arc, LazyLock, Mutex, RwLock},
        time,
    };

    use ax_tracepoint::{ExtTracePoint, TraceCmdLineCache, define_event_trace};

    pub static TRACE_RAW_PIPE: Mutex<ax_tracepoint::TracePipeRaw> =
        Mutex::new(ax_tracepoint::TracePipeRaw::new(1024));

    pub static TRACE_CMDLINE_CACHE: LazyLock<Mutex<TraceCmdLineCache>> = LazyLock::new(|| {
        Mutex::new(ax_tracepoint::TraceCmdLineCache::new(
            NonZero::new(1024).unwrap(),
        ))
    });

    pub static EXT_TRACEPOINTS: RwLock<BTreeMap<u32, ExtTracePoint<Kops>>> =
        RwLock::new(BTreeMap::new());

    pub struct Kops;

    impl ax_tracepoint::KernelTraceOps for Kops {
        fn current_pid() -> u32 {
            0xff
        }

        fn trace_pipe_push_raw_record(buf: &[u8]) {
            let time = time::SystemTime::now()
                .duration_since(time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            let cpu_id = 8;
            let mut pipe = TRACE_RAW_PIPE.lock().unwrap();
            pipe.push_record(time, cpu_id, buf.to_vec());
        }

        fn trace_cmdline_push(pid: u32) {
            let mut cache = TRACE_CMDLINE_CACHE.lock().unwrap();
            cache.insert(pid, "test_process");
        }

        fn read_tracepoint_state<R>(id: u32, f: impl FnOnce(&ExtTracePoint<Self>) -> R) -> R {
            let states = EXT_TRACEPOINTS.read().unwrap();
            states
                .get(&id)
                .map(f)
                .unwrap_or_else(|| panic!("Tracepoint state not found for ID: {}", id))
        }

        fn write_tracepoint_state<R>(id: u32, f: impl FnOnce(&mut ExtTracePoint<Self>) -> R) -> R {
            let mut states = EXT_TRACEPOINTS.write().unwrap();
            let state = states
                .get_mut(&id)
                .unwrap_or_else(|| panic!("Tracepoint state not found for ID: {}", id));
            let result = f(state);
            state.trace_point().set_callback_gate(state.has_callbacks());
            result
        }
    }

    #[repr(C)]
    #[derive(Debug)]
    #[allow(clippy::redundant_allocation)]
    struct TestS {
        a: u32,
        b: Box<Arc<u32>>,
    }

    define_event_trace!(
        TEST,
        TP_kops(Kops),
        TP_system(tracepoint_test),
        TP_PROTO(a: u32, b: &TestS),
        TP_STRUCT__entry {
            a: u32,
            pad: [u8; 4],
            b: u32
        },
        TP_fast_assign {
            a: a,
            pad: [0; 4],
            b: *b.b.deref().deref()
        },
        TP_ident(__entry),
        TP_printk({
            let arg1 = __entry.a;
            let arg2 = __entry.b;
            format!("Hello from tracepoint! a={:?}, b={}", arg1, arg2)
        })
    );

    define_event_trace!(
        TEST2,
        TP_kops(Kops),
        TP_system(tracepoint_test),
        TP_PROTO(a: u32, b: u32),
        TP_STRUCT__entry { a: u32, b: u32 },
        TP_fast_assign { a: a, b: b },
        TP_ident(__entry),
        TP_printk(format_args!(
            "Hello from tracepoint! a={}, b={}",
            __entry.a, __entry.b
        ))
    );

    pub fn test_trace(a: u32, b: u32) {
        let x = TestS {
            a,
            b: Box::new(Arc::new(b)),
        };
        trace_TEST(a, &x);
        trace_TEST2(a, b);
        println!(
            "Tracepoint TEST called with a={}, b={}, x ptr={:p}",
            a, b, &x
        );
    }
}

fn print_trace_records(
    tracepoint_map: &TracePointMap<Kops>,
    trace_cmdline_cache: &TraceCmdLineCache,
) {
    use ax_tracepoint::TracePipeOps;
    let mut snapshot = TRACE_RAW_PIPE.lock().unwrap().snapshot();
    print!("{}", snapshot.default_fmt_str());
    loop {
        let mut flag = false;
        if let Some(event) = snapshot.peek() {
            let trace_str = TraceEntryParser::parse(tracepoint_map, trace_cmdline_cache, event)
                .expect("example emitted an invalid trace record");
            print!("{}", trace_str);
            flag = true;
        }
        if flag {
            snapshot.pop();
        } else {
            break;
        }
    }
}

fn perf_event_callback() -> TraceCallbackType {
    let callback = Arc::new(TraceEventFunc::new(
        Box::new(|entry, _data| {
            println!("FakeEventCallback called with entry: {}", entry.len());
        }),
        Box::new(()),
    ));
    callback.set_perf_enable(true);
    TraceCallbackType::Event(callback)
}

fn raw_event_callback() -> TraceCallbackType {
    let callback = RawTraceEventFunc::new(
        Box::new(|args, _data| {
            println!("FakeEventCallback (raw) called with args: {:x?}", args);
        }),
        Box::new(()),
    );
    TraceCallbackType::RawEvent(Arc::new(callback))
}

fn main() {
    env_logger::try_init_from_env(env_logger::Env::default().default_filter_or("debug"))
        .expect("Failed to initialize logger");

    // First, initialize the tracepoint metadata and runtime event states.
    let (tracepoint_map, ext_tps) = global_init_events::<Kops>().unwrap();

    let ext_tps = ext_tps
        .into_iter()
        .map(|ext_tp| (ext_tp.trace_point().id(), ext_tp))
        .collect::<BTreeMap<_, _>>();

    *EXT_TRACEPOINTS.write().unwrap() = ext_tps;

    println!("---Before enabling tracepoints---");
    tracepoint_example::test_trace(1, 2);
    tracepoint_example::test_trace(3, 4);
    print_trace_records(
        &tracepoint_map,
        &tracepoint_example::TRACE_CMDLINE_CACHE.lock().unwrap(),
    );

    println!();

    let perf_event_callback = perf_event_callback();

    let raw_event_callback = raw_event_callback();

    for (event_id, tp) in tracepoint_map.iter() {
        Kops::write_tracepoint_state(*event_id, |ext_tp| {
            let enable_file = TracePointEnableFile::new();
            enable_file.write(ext_tp, '1');
            ext_tp.register(perf_event_callback.clone());
            ext_tp.register(raw_event_callback.clone());

            let mut filter_file = TraceFilterFile::new();
            filter_file
                .write(ext_tp, "(a > 8 && a<=10) || b >5")
                .unwrap();
        });

        let schema = tp.schema();
        println!("Schema for {}::{}: {:#?}", tp.system(), tp.name(), schema);
        println!("Enabled tracepoint: {}::{}", tp.system(), tp.name());
    }

    println!("---After enabling tracepoints---");
    tracepoint_example::test_trace(1, 2);
    tracepoint_example::test_trace(9, 2); // should match
    tracepoint_example::test_trace(3, 4);
    tracepoint_example::test_trace(10, 4); // should match
    tracepoint_example::test_trace(11, 6); // should match

    print_trace_records(
        &tracepoint_map,
        &tracepoint_example::TRACE_CMDLINE_CACHE.lock().unwrap(),
    );

    for tracepoint in tracepoint_map.values() {
        println!("{}", tracepoint.print_fmt());
    }
}
