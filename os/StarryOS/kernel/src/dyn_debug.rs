use ax_memory_addr::VirtAddr;
use ax_task::current;
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
        current().id().as_u64()
    }
}

/// See [ax_debug_fn] for usage.
///
/// # Note
/// This macro don't depend on the derive macro `#[ddebug::named]`, so the 'f' flag can't be used to print the function name.
#[macro_export]
macro_rules! ax_debug {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {{
        ddebug::pr_debug!($crate::dyn_debug::DynamicDebugOps, $fmt $(, $arg)*);
    }};
}

/// A debug print macro that also prints the function name. This is useful for debugging dynamic debug sites, as it can help identify which site is being hit.
///
/// # Note
/// This macro depends on the derive macro `#[ddebug::named]` to work, which will set the function name for the debug site.
#[macro_export]
macro_rules! ax_debug_fn {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {{
        ddebug::pr_debug_fn!($crate::dyn_debug::DynamicDebugOps, $fmt $(, $arg)*);
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
