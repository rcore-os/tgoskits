use crate::AxTestDescriptor;

/// Function pointer type for per-module test hooks.
pub type AxTestModHookFn = fn(AxTestDescriptor);

/// Linker-collected descriptor for a module's optional init/exit hooks.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct AxTestModHookDescriptor {
    /// Module path this hook descriptor belongs to.
    pub module: &'static str,
    /// Optional pre-test hook.
    pub init: Option<AxTestModHookFn>,
    /// Optional post-test hook.
    pub exit: Option<AxTestModHookFn>,
}

impl AxTestModHookDescriptor {
    /// Construct a new immutable module hook descriptor.
    pub const fn new(
        module: &'static str,
        init: Option<AxTestModHookFn>,
        exit: Option<AxTestModHookFn>,
    ) -> Self {
        Self { module, init, exit }
    }
}

fn find_module_hook(module: &str) -> Option<&'static AxTestModHookDescriptor> {
    #[allow(improper_ctypes)]
    unsafe extern "C" {
        #[link_name = "__axtest_mod_hooks_start"]
        static _axtest_mod_hooks_start: AxTestModHookDescriptor;
        #[link_name = "__axtest_mod_hooks_end"]
        static _axtest_mod_hooks_end: AxTestModHookDescriptor;
    }

    unsafe {
        let start = core::ptr::addr_of!(_axtest_mod_hooks_start);
        let end = core::ptr::addr_of!(_axtest_mod_hooks_end);
        if start.is_null() || end.is_null() || start >= end {
            return None;
        }

        let hooks = core::slice::from_raw_parts(start, end.offset_from(start) as usize);
        hooks.iter().find(|h| h.module == module)
    }
}

/// Invoke module init hook if this module registered one.
pub fn call_module_init(module: &str, sym: AxTestDescriptor) {
    if let Some(hook) = find_module_hook(module).and_then(|h| h.init) {
        hook(sym);
    }
}

/// Invoke module exit hook if this module registered one.
pub fn call_module_exit(module: &str, sym: AxTestDescriptor) {
    if let Some(hook) = find_module_hook(module).and_then(|h| h.exit) {
        hook(sym);
    }
}
