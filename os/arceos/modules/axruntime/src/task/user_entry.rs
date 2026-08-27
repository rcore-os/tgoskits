//! Safe, task-bound user execution boundary.

use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use ax_hal::cpu::uspace::{ReturnReason, UserContext};
use ax_task::{TaskError, runtime::RuntimeStatus};

use super::{
    context::{RuntimeUserBinding, bind_current_user_context, validate_current_user_context},
    runtime_status_error, with_current_cpu_pin,
};

/// A user register image bound to the scheduler execution context that owns it.
///
/// The wrapper is deliberately neither `Send` nor `Sync`: a saved register
/// image may move only through the scheduler's architecture context, while the
/// live execution capability stays in the current task.
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_runtime::task::UserExecutionContext>();
/// ```
pub struct UserExecutionContext {
    registers: UserContext,
    binding: RuntimeUserBinding,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl UserExecutionContext {
    /// Binds a saved user register image to the current scheduler context.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::UnsafeContext`] before switch-tail completion or
    /// while an IRQ/preemption scope is active, and a runtime error when the
    /// current task does not own a published user address space.
    pub fn bind(registers: UserContext) -> Result<Self, TaskError> {
        if crate::guard::validate_schedule_context(ax_task::runtime::RuntimeScheduleOrigin::Preempt)
            != RuntimeStatus::Success
        {
            return Err(TaskError::UnsafeContext);
        }

        let irq = crate::sync::IrqSaveGuard::new();
        let selected_address_space = ax_task::current_address_space_handle()?;
        let binding = unsafe {
            // SAFETY: `irq` prevents migration and owns the current CPU for
            // both the runtime-context and active-mm observations.
            with_current_cpu_pin(|pin| {
                let binding = bind_current_user_context(pin)?;
                super::address_space::validate_current_user_address_space(
                    pin,
                    selected_address_space,
                )?;
                Ok::<_, RuntimeStatus>(binding)
            })
        }
        .map_err(runtime_status_error)?;
        drop(irq);

        Ok(Self {
            registers,
            binding,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Drains scheduler work, validates the current task/mm/root transaction,
    /// and enters userspace without an intervening safe-code window.
    ///
    /// This method returns after a syscall, exception, page fault, or hardware
    /// interrupt restores the kernel continuation.
    pub fn enter(&mut self) -> Result<ReturnReason, TaskError> {
        crate::guard::prepare_user_return()?;

        match self.validate_prepared_entry() {
            Ok(()) => Ok(PreparedUserEntry {
                registers: &mut self.registers,
                _not_send_or_sync: PhantomData,
            }
            .enter()),
            Err(error) => {
                // bind/prepare require IRQ-enabled task context, so a failed
                // final validation restores exactly that entry state.
                ax_hal::asm::enable_irqs();
                Err(error)
            }
        }
    }

    fn validate_prepared_entry(&self) -> Result<(), TaskError> {
        if crate::guard::validate_prepared_user_entry() != RuntimeStatus::Success {
            return Err(TaskError::UnsafeContext);
        }
        if !self.registers.is_user_entry_state_valid() {
            return Err(TaskError::UnsafeContext);
        }
        let selected_address_space = ax_task::current_address_space_handle()?;
        unsafe {
            // SAFETY: prepare_user_return left local IRQs disabled, so the
            // current context, CPU and active-mm publications cannot move.
            with_current_cpu_pin(|pin| {
                validate_current_user_context(pin, &self.binding)?;
                super::address_space::validate_current_user_address_space(
                    pin,
                    selected_address_space,
                )
            })
        }
        .map_err(runtime_status_error)
    }
}

impl Deref for UserExecutionContext {
    type Target = UserContext;

    fn deref(&self) -> &Self::Target {
        &self.registers
    }
}

impl DerefMut for UserExecutionContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.registers
    }
}

/// One non-replayable borrow spanning final validation and raw user entry.
#[must_use = "prepared user entry must be consumed immediately"]
struct PreparedUserEntry<'entry> {
    registers: &'entry mut UserContext,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl PreparedUserEntry<'_> {
    fn enter(self) -> ReturnReason {
        // SAFETY: construction is private and immediately follows the complete
        // runtime validation while local IRQs remain disabled. Consuming self
        // prevents replay or code insertion through a public guard API.
        unsafe { self.registers.run_unchecked() }
    }
}
