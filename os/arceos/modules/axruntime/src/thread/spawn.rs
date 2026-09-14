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
    pub(super) address_space: TaskAddressSpace,
    #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
    pub(super) fp_state: Option<ax_hal::cpu::registers::FpState>,
    #[cfg(all(not(target_arch = "riscv64"), feature = "fp-simd", feature = "uspace"))]
    pub(super) inherit_current_fp: bool,
}
impl UserContextOptions {
    /// Creates a user context with the architecture's initial FP state.
    pub fn new(address_space: TaskAddressSpace) -> Self {
        Self {
            address_space,
            #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
            fp_state: None,
            #[cfg(all(not(target_arch = "riscv64"), feature = "fp-simd", feature = "uspace"))]
            inherit_current_fp: false,
        }
    }
    /// Supplies the child's RISC-V floating-point register image.
    #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
    pub fn with_fp_state(mut self, state: ax_hal::cpu::registers::FpState) -> Self {
        self.fp_state = Some(state);
        self
    }
    /// Captures the calling thread's user FP state during context preparation.
    #[cfg(all(not(target_arch = "riscv64"), feature = "fp-simd", feature = "uspace"))]
    pub fn inherit_current_fp(mut self) -> Self {
        self.inherit_current_fp = true;
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
    #[cfg(all(not(target_arch = "riscv64"), feature = "fp-simd", feature = "uspace"))]
    if options.inherit_current_fp {
        context::validate_current_user_fp_clone_context()?;
    }
    // SAFETY: the resource factory installs the provided trampoline, and its
    // rollback owns every partial allocation until returning a complete bundle.
    unsafe {
        builder.prepare_with(entry, |request, trampoline| {
            create_user_resources(request, trampoline, options)
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
