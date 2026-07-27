use ax_memory_addr::VirtAddr;
use ax_std::os::arceos::task::current_thread_id;
use ddebug::{ControlFile, DebugOps};

pub struct DynamicDebugOps;

impl DebugOps for DynamicDebugOps {
    fn write_kernel_text(addr: *mut u8, data: &[u8]) {
        crate::mm::write_kernel_text(VirtAddr::from_mut_ptr_of(addr), data)
            .expect("Failed to write kernel text");
    }

    fn emit(line: &str) {
        ax_print!("{}", line);
    }

    fn thread_id() -> u64 {
        current_thread_id().map_or(0, |thread| thread.as_u64())
    }
}

/// Dynamic debug macro. When `dynamic_debug` feature is enabled,
/// uses per-callsite static key for runtime control via `/proc/dynamic_debug/control`.
/// Otherwise falls back to `log::debug!`.
///
/// # Note
/// This macro doesn't depend on the derive macro `#[ddebug::named]`, so the 'f' flag can't be used to print the function name.
#[cfg(feature = "dynamic_debug")]
#[macro_export]
macro_rules! debug {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {{
        ddebug::pr_debug!($crate::dyn_debug::DynamicDebugOps, $fmt $(, $arg)*);
    }};
}

/// Dynamic debug macro. When `dynamic_debug` feature is enabled,
/// uses per-callsite static key for runtime control via `/proc/dynamic_debug/control`, and also prints the function name of the callsite.
/// Otherwise falls back to `log::debug!`.
///
/// # Note
/// This macro depends on the derive macro `#[ddebug::named]` to work, which will set the function name for the debug site.
#[cfg(feature = "dynamic_debug")]
#[macro_export]
macro_rules! debug_fn {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {{
        ddebug::pr_debug_fn!($crate::dyn_debug::DynamicDebugOps, $fmt $(, $arg)*);
    }};
}

/// When `dynamic_debug` feature is disabled, `debug!` and `debug_fn!` both fall back to `log::debug!`.
#[cfg(not(feature = "dynamic_debug"))]
#[macro_export]
macro_rules! debug_fn {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {{
        ax_log::debug!($fmt $(, $arg)*);
    }};
}

/// Initialize dynamic debug subsystem.
/// This should be called after static keys are initialized, and before any dynamic debug site is hit.
pub fn dynamic_debug_init() -> ControlFile<DynamicDebugOps> {
    info!("debug_init: initializing dynamic debug sites");
    let ctl = ddebug::dynamic_debug_init::<DynamicDebugOps>();
    let site_count = ctl.site_count();
    info!("debug_init: found {site_count} dynamic debug sites");
    ctl
}
