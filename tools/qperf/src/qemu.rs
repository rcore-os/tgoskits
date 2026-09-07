//! QEMU API v7 entrypoints and callback ownership for this profiler.

pub(super) mod ffi;

use std::{
    collections::HashMap,
    ffi::{CStr, c_char, c_int, c_uint, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
};

use anyhow::{Context, bail};

use crate::{profiler::Profiler, reg};

#[derive(Debug)]
pub enum Value {
    Integer(i64),
    String(String),
}

pub struct Args {
    pub parsed: HashMap<String, Value>,
}

impl Args {
    fn parse(raw: &[String]) -> anyhow::Result<Self> {
        let mut parsed = HashMap::new();
        for argument in raw {
            let (key, value) = argument.split_once('=').context("expected key=value")?;
            let value = match value.parse() {
                Ok(integer) => Value::Integer(integer),
                Err(_) => Value::String(value.to_owned()),
            };
            parsed.insert(key.to_owned(), value);
        }
        Ok(Self { parsed })
    }
}

struct Runtime {
    profiler: Arc<Profiler>,
    // Arc allocations keep userdata stable when this vector reallocates.
    sites: Mutex<Vec<Arc<SampleSite>>>,
}

struct SampleSite {
    profiler: Arc<Profiler>,
    ip: u64,
}

#[unsafe(export_name = "qemu_plugin_version")]
pub static PLUGIN_VERSION: c_int = ffi::API_VERSION;

/// Install the profiler after QEMU has checked the exported API version.
///
/// # Safety
/// QEMU supplies a live `Info`, and `argc` live, NUL-terminated entries in `argv`.
/// Neither the info block nor its strings are retained after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qemu_plugin_install(
    id: u64,
    info: *const ffi::Info,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if info.is_null() || argc < 0 || (argc != 0 && argv.is_null()) {
            bail!("invalid QEMU install arguments");
        }
        // SAFETY: QEMU keeps info and its target string live throughout installation.
        let info = unsafe { &*info };
        if info.target_name.is_null()
            || info.version.min > PLUGIN_VERSION
            || info.version.cur < PLUGIN_VERSION
        {
            bail!("QEMU does not support qperf's plugin API v{PLUGIN_VERSION}");
        }
        // SAFETY: The target string is non-null, terminated, and owned by QEMU.
        let target = unsafe { CStr::from_ptr(info.target_name) }.to_str()?;
        let mut raw = Vec::new();
        for index in 0..argc as usize {
            // SAFETY: The loader supplies argc initialized pointers, checked above.
            let arg = unsafe { *argv.add(index) };
            if arg.is_null() {
                bail!("null QEMU plugin argument");
            }
            // SAFETY: Each non-null argv entry is a live NUL-terminated string.
            raw.push(unsafe { CStr::from_ptr(arg) }.to_str()?.to_owned());
        }
        let args = Args::parse(&raw)?;
        let profiler = Arc::new(Profiler::start(&args, target)?);
        eprintln!("QPerf loaded: target={target} plugin_api={PLUGIN_VERSION}");
        let runtime = Box::into_raw(Box::new(Runtime {
            profiler,
            sites: Mutex::default(),
        }))
        .cast();
        // SAFETY: Runtime has a stable allocation. Only on_exit reclaims it, once
        // execution has stopped. All callbacks have the v7 userdata signatures.
        // Registration is infallible; no failure path follows this publication.
        unsafe {
            ffi::qemu_plugin_register_vcpu_init_cb(id, on_vcpu_init, runtime);
            ffi::qemu_plugin_register_vcpu_exit_cb(id, on_vcpu_exit, runtime);
            ffi::qemu_plugin_register_vcpu_tb_trans_cb(id, on_translate, runtime);
            ffi::qemu_plugin_register_flush_cb(id, on_flush, runtime);
            ffi::qemu_plugin_register_atexit_cb(id, on_exit, runtime);
        }
        Ok::<(), anyhow::Error>(())
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            eprintln!("qperf install failed: {error:#}");
            -1
        }
        Err(_) => {
            eprintln!("qperf install panicked");
            -1
        }
    }
}

unsafe extern "C" fn on_vcpu_init(cpu: c_uint, userdata: *mut c_void) {
    // SAFETY: Installation owns this Runtime until atexit, after vCPU callbacks.
    let runtime = unsafe { &*userdata.cast::<Runtime>() };
    if runtime.profiler.needs_registers() {
        // SAFETY: QEMU invokes this on the initialized vCPU's own thread.
        if let Err(error) = unsafe { reg::init(cpu) } {
            eprintln!("qperf register initialization failed: {error:#}");
            std::process::abort();
        }
    }
}

unsafe extern "C" fn on_vcpu_exit(cpu: c_uint, _userdata: *mut c_void) {
    reg::clear(cpu);
}

unsafe extern "C" fn on_translate(tb: *mut ffi::TranslationBlock, userdata: *mut c_void) {
    // SAFETY: Runtime remains owned until atexit. The TB and its instructions are
    // valid only in this callback; none of their pointers escape it.
    let runtime = unsafe { &*userdata.cast::<Runtime>() };
    let profiler = &runtime.profiler;
    let flags = if profiler.needs_registers() {
        ffi::R_REGS
    } else {
        ffi::NO_REGS
    };
    let mut sites = runtime.sites.lock().expect("qperf site registry poisoned");
    let mut add_site = |ip| {
        let ip = profiler.sample_ip_for(ip)?;
        let site = Arc::new(SampleSite {
            profiler: profiler.clone(),
            ip,
        });
        let ptr = Arc::as_ptr(&site).cast_mut().cast();
        sites.push(site);
        Some(ptr)
    };
    // SAFETY: QEMU supplies the live TB; indices are bounded by its instruction
    // count. Sites are retained until flush/atexit, when QEMU stops execution.
    unsafe {
        if profiler.instruction_sampling() {
            for index in 0..ffi::qemu_plugin_tb_n_insns(tb) {
                let insn = ffi::qemu_plugin_tb_get_insn(tb, index);
                if let Some(site) = add_site(ffi::qemu_plugin_insn_vaddr(insn)) {
                    ffi::qemu_plugin_register_vcpu_insn_exec_cb(insn, on_execute, flags, site);
                }
            }
        } else if let Some(site) = add_site(ffi::qemu_plugin_tb_vaddr(tb)) {
            ffi::qemu_plugin_register_vcpu_tb_exec_cb(tb, on_execute, flags, site);
        }
    }
}

unsafe extern "C" fn on_execute(cpu: c_uint, userdata: *mut c_void) {
    // SAFETY: This site belongs to an installed TB/insn. QEMU stops all execution
    // before the flush callback releases the sites; atexit follows execution.
    let site = unsafe { &*userdata.cast::<SampleSite>() };
    // SAFETY: This runs on the current vCPU with R_REGS whenever FP is enabled.
    if unsafe { site.profiler.sample(cpu, site.ip) }.is_err() {
        site.profiler.record_failure();
    }
}

unsafe extern "C" fn on_flush(userdata: *mut c_void) {
    // SAFETY: QEMU guarantees all CPUs are stopped and old TBs cannot execute.
    let runtime = unsafe { &*userdata.cast::<Runtime>() };
    runtime
        .sites
        .lock()
        .expect("qperf site registry poisoned")
        .clear();
}

unsafe extern "C" fn on_exit(userdata: *mut c_void) {
    // SAFETY: QEMU calls atexit exactly once after execution finishes. This takes
    // back the sole Box transferred at installation; no callback may use it again.
    let runtime = unsafe { Box::from_raw(userdata.cast::<Runtime>()) };
    drop(runtime);
    // Dropping the final Profiler Arc drains and waits for its writer via Shutdown.
}
