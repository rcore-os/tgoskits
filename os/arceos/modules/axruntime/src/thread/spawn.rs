//! ArceOS stack defaults and architecture-specific user context assembly.

use ax_task::thread::{PreparedThread, ThreadBuilder};

use super::*;

/// Applies ArceOS stack and guard-page defaults to the common thread builder.
pub fn builder(name: String) -> ThreadBuilder {
    ThreadBuilder::new(name)
        .stack_size(default_task_stack_size())
        .guard_size(if cfg!(feature = "stack-guard-page") {
            PAGE_SIZE
        } else {
            0
        })
}

/// Initial user address space and architecture-specific floating-point state.
pub struct UserContextOptions {
    state: InitialContextState,
}
impl UserContextOptions {
    /// Creates a user context with the architecture's initial FP state.
    pub fn new(address_space: TaskAddressSpace) -> Self {
        Self {
            state: InitialContextState::user(address_space),
        }
    }
    /// Supplies the child's RISC-V floating-point register image.
    #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
    pub fn with_fp_state(mut self, state: ax_hal::cpu::FpState) -> Self {
        self.state.fp_state = Some(state);
        self
    }
    /// Captures the calling thread's x86 user xstate during context preparation.
    #[cfg(all(target_arch = "x86_64", feature = "fp-simd", feature = "uspace"))]
    pub fn inherit_current_fp(mut self) -> Self {
        self.state.inherit_current_fp();
        self
    }
}

/// Prepares user architecture resources without publishing or activating the task.
///
/// # Safety
/// `entry` must enter user mode only with the supplied address-space ownership and
/// initialized architecture state. Inherited FP state must belong to the caller.
pub unsafe fn prepare_user_thread(
    builder: ThreadBuilder,
    entry: impl FnOnce() + Send + 'static,
    options: UserContextOptions,
) -> Result<PreparedThread, TaskError> {
    #[cfg(all(target_arch = "x86_64", feature = "fp-simd", feature = "uspace"))]
    if options.state.inherits_current_fp() {
        context::validate_current_user_fp_clone_context()?;
    }
    // SAFETY: the resource factory installs the provided trampoline, and its
    // rollback owns every partial allocation until returning a complete bundle.
    unsafe {
        builder.prepare_with(entry, |request, trampoline| {
            create_thread_resources(request, trampoline, options.state)
        })
    }
}

/// Exits an ArceOS task, preserving the primary bootstrap termination policy.
pub fn exit_current(code: i32) -> ! {
    if primary_bootstrap_thread() == current_thread_id().ok() {
        crate::terminate();
    }
    ax_task::thread::current::exit_current(code)
}
