use core::{marker::PhantomData, mem::ManuallyDrop, pin::Pin, ptr::NonNull};

use crate::{
    ContextSwitchError, CpuBindingEpoch, CpuLocalError, CpuPin, ExecutionContextHeader,
    current_context,
};

/// Prepared current-context publication owned by the final switch tail.
#[must_use = "dropping an uncommitted switch rolls back the next CPU binding"]
pub struct PreparedContextSwitch<'switch> {
    next: NonNull<ExecutionContextHeader>,
    next_epoch: CpuBindingEpoch,
    current_context: usize,
    area: crate::CpuAreaRef,
    _scope: PhantomData<&'switch mut &'switch ()>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl PreparedContextSwitch<'_> {
    /// Returns the exact next header bound by this transaction.
    #[doc(hidden)]
    pub const fn next_header(&self) -> NonNull<ExecutionContextHeader> {
        self.next
    }

    /// Publishes the prepared CPU anchor slot immediately before naked switch.
    ///
    /// # Safety
    ///
    /// The caller serialization and IRQ exclusion used during preparation
    /// must still be active. The caller must enter the architecture switch
    /// without performing fallible or ownership-sensitive Rust work.
    #[doc(hidden)]
    #[inline(always)]
    pub unsafe fn commit(self) {
        // Disarm rollback before publication. After the final call below there
        // is no destructor state update or ownership-sensitive Rust work; the
        // architecture wrapper enters its naked switch tail immediately.
        let prepared = ManuallyDrop::new(self);
        unsafe { crate::register::commit_current_context(prepared.area, prepared.current_context) };
    }
}

impl Drop for PreparedContextSwitch<'_> {
    fn drop(&mut self) {
        // SAFETY: an uncommitted token still owns the next binding, and its
        // invariant lifetime keeps the caller's critical section live. The
        // preparation contract keeps the pinned header alive until this drop.
        let next = unsafe { Pin::new_unchecked(self.next.as_ref()) };
        if unsafe { next.unbind_cpu(self.next_epoch) }.is_err() {
            panic!("prepared context-switch rollback lost the next CPU binding");
        }
    }
}

/// Opaque previous-context binding consumed by the incoming switch tail.
#[must_use = "the incoming context must withdraw the previous CPU binding"]
#[derive(Debug)]
pub struct PreviousContextBinding {
    previous: NonNull<ExecutionContextHeader>,
    epoch: CpuBindingEpoch,
}

impl PreviousContextBinding {
    /// Withdraws the exact previous binding after architecture registers have
    /// switched to the incoming context.
    ///
    /// # Errors
    ///
    /// Returns [`ContextSwitchError::PreviousContextMismatch`] if `previous`
    /// differs from the prepared context, or
    /// [`ContextSwitchError::StalePreviousBinding`] for an obsolete tail.
    ///
    /// # Safety
    ///
    /// The incoming switch tail must be the sole owner of this token and the
    /// previous context allocation must remain pinned and alive.
    pub unsafe fn finish(
        self,
        previous: Pin<&ExecutionContextHeader>,
    ) -> Result<(), ContextSwitchError> {
        if previous.as_non_null() != self.previous {
            return Err(ContextSwitchError::PreviousContextMismatch);
        }
        unsafe { previous.unbind_cpu(self.epoch) }
    }
}

/// Validates and binds a complete execution-context switch transaction.
///
/// All fallible validation occurs before the returned prepared token can be
/// committed. Dropping the prepared token automatically rolls back `next`.
///
/// # Safety
///
/// The caller must own the IRQ-disabled context-switch path. Both headers
/// must remain pinned and alive through the raw switch and incoming tail.
pub unsafe fn prepare_context_switch<'switch>(
    pin: &'switch CpuPin<'_>,
    previous: Pin<&ExecutionContextHeader>,
    next: Pin<&ExecutionContextHeader>,
) -> Result<(PreparedContextSwitch<'switch>, PreviousContextBinding), ContextSwitchError> {
    let published = current_context(pin).map_err(|error| match error {
        CpuLocalError::CurrentContextMismatch => ContextSwitchError::CurrentContextMismatch,
        other => ContextSwitchError::CpuLocal(other),
    })?;
    if published != previous.as_non_null() {
        return Err(ContextSwitchError::CurrentContextMismatch);
    }
    let previous_binding = previous
        .cpu_binding()
        .filter(|binding| binding.area == pin.area())
        .ok_or(ContextSwitchError::CurrentContextMismatch)?;
    let next_epoch = unsafe { next.bind_cpu(pin.area()) }?;
    Ok((
        PreparedContextSwitch {
            next: next.as_non_null(),
            next_epoch,
            current_context: next.as_non_null().as_ptr() as usize,
            area: pin.area(),
            _scope: PhantomData,
            _not_send_or_sync: PhantomData,
        },
        PreviousContextBinding {
            previous: previous.as_non_null(),
            epoch: previous_binding.epoch,
        },
    ))
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use core::mem::MaybeUninit;

    use super::*;
    use crate::{
        CpuAreaPrefix, CpuAreaRef, CpuIndex, install_bootstrap_context, install_cpu_area,
        with_cpu_pin,
    };

    fn on_fresh_modeled_cpu(operation: impl FnOnce(CpuAreaRef) + Send + 'static) {
        std::thread::spawn(move || {
            let storage = Box::leak(Box::new(MaybeUninit::<CpuAreaPrefix>::uninit()));
            let base = storage.as_mut_ptr() as usize;
            storage.write(CpuAreaPrefix::initialize(CpuIndex::try_from(0).unwrap(), base).unwrap());
            // SAFETY: the leaked prefix is initialized and remains mapped for
            // the complete process lifetime.
            let area = unsafe { CpuAreaRef::from_initialized_base(base) }.unwrap();
            // SAFETY: this fresh host thread models one offline CPU and owns
            // the completed area exclusively during register installation.
            unsafe { install_cpu_area(area) }.unwrap();
            operation(area);
        })
        .join()
        .unwrap();
    }

    fn context_header() -> Pin<Box<ExecutionContextHeader>> {
        Box::pin(ExecutionContextHeader::new())
    }

    #[test]
    fn abandoned_prepare_rolls_back_next_binding() {
        on_fresh_modeled_cpu(|area| {
            let previous = context_header();
            let next = context_header();

            // SAFETY: the modeled CPU cannot migrate or receive interrupts.
            unsafe {
                with_cpu_pin(|pin| {
                    install_bootstrap_context(pin, previous.as_ref()).unwrap();
                    let (prepared, _previous_binding) =
                        prepare_context_switch(pin, previous.as_ref(), next.as_ref()).unwrap();
                    assert_eq!(current_context(pin), Ok(previous.as_ref().as_non_null()));
                    assert_eq!(next.cpu_area(), Some(area));

                    drop(prepared);

                    assert_eq!(current_context(pin), Ok(previous.as_ref().as_non_null()));
                    assert_eq!(next.cpu_area(), None);
                })
            }
            .unwrap();
        });
    }

    #[test]
    fn prepare_reports_the_domain_mismatch_before_binding_next() {
        on_fresh_modeled_cpu(|_| {
            let published = context_header();
            let wrong_previous = context_header();
            let next = context_header();

            // SAFETY: the modeled CPU cannot migrate or receive interrupts.
            unsafe {
                with_cpu_pin(|pin| {
                    install_bootstrap_context(pin, published.as_ref()).unwrap();
                    let result =
                        prepare_context_switch(pin, wrong_previous.as_ref(), next.as_ref());
                    assert!(matches!(
                        result,
                        Err(ContextSwitchError::CurrentContextMismatch)
                    ));
                    assert_eq!(next.cpu_area(), None);
                })
            }
            .unwrap();
        });
    }

    #[test]
    fn publication_precedes_incoming_unbind() {
        on_fresh_modeled_cpu(|area| {
            let previous = context_header();
            let next = context_header();

            // SAFETY: this host model serializes the entire switch. Returning
            // from `commit` represents resuming after the naked switch tail.
            unsafe {
                with_cpu_pin(|pin| {
                    install_bootstrap_context(pin, previous.as_ref()).unwrap();
                    let (prepared, previous_binding) =
                        prepare_context_switch(pin, previous.as_ref(), next.as_ref()).unwrap();

                    assert_eq!(current_context(pin), Ok(previous.as_ref().as_non_null()));
                    assert_eq!(previous.cpu_area(), Some(area));
                    assert_eq!(next.cpu_area(), Some(area));

                    prepared.commit();

                    assert_eq!(current_context(pin), Ok(next.as_ref().as_non_null()));
                    assert_eq!(previous.cpu_area(), Some(area));
                    previous_binding.finish(previous.as_ref()).unwrap();
                    assert_eq!(previous.cpu_area(), None);
                    assert_eq!(next.cpu_area(), Some(area));
                })
            }
            .unwrap();
        });
    }

    #[test]
    fn stale_epoch_cannot_unbind_a_new_binding() {
        on_fresh_modeled_cpu(|area| {
            let header = context_header();
            // SAFETY: this fresh modeled CPU exclusively owns the header.
            let stale = unsafe { header.as_ref().bind_cpu(area) }.unwrap();
            unsafe { header.as_ref().unbind_cpu(stale) }.unwrap();
            let current = unsafe { header.as_ref().bind_cpu(area) }.unwrap();

            assert_eq!(
                unsafe { header.as_ref().unbind_cpu(stale) },
                Err(ContextSwitchError::StalePreviousBinding)
            );
            assert_eq!(header.cpu_area(), Some(area));
            unsafe { header.as_ref().unbind_cpu(current) }.unwrap();
        });
    }
}
