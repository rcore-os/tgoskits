use core::{marker::PhantomData, mem::ManuallyDrop, pin::Pin, ptr::NonNull};

use crate::{
    CpuBindingEpoch, CpuLocalError, CpuPin, CurrentThreadHeader, ThreadSwitchError, current_thread,
};

/// Prepared current-thread publication owned by the final context-switch tail.
#[must_use = "dropping an uncommitted switch rolls back the next CPU binding"]
pub struct PreparedThreadSwitch<'switch> {
    next: NonNull<CurrentThreadHeader>,
    next_epoch: CpuBindingEpoch,
    current_thread: usize,
    area: crate::CpuAreaRef,
    _scope: PhantomData<&'switch mut &'switch ()>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl PreparedThreadSwitch<'_> {
    /// Returns the exact next header bound by this transaction.
    #[doc(hidden)]
    pub const fn next_header(&self) -> NonNull<CurrentThreadHeader> {
        self.next
    }

    /// Publishes the prepared CPU runtime slot immediately before naked switch.
    ///
    /// # Safety
    ///
    /// The scheduler serialization and IRQ exclusion used during preparation
    /// must still be active. The caller must enter the architecture switch
    /// without performing fallible or ownership-sensitive Rust work.
    #[doc(hidden)]
    #[inline(always)]
    pub unsafe fn commit(self) {
        // Disarm rollback before publication. After the final call below there
        // is no destructor state update or ownership-sensitive Rust work; the
        // architecture wrapper enters its naked switch tail immediately.
        let prepared = ManuallyDrop::new(self);
        unsafe { crate::register::commit_current_thread(prepared.area, prepared.current_thread) };
    }
}

impl Drop for PreparedThreadSwitch<'_> {
    fn drop(&mut self) {
        // SAFETY: an uncommitted token still owns the next binding, and its
        // invariant lifetime keeps the scheduler critical section live. The
        // preparation contract keeps the pinned header alive until this drop.
        let next = unsafe { Pin::new_unchecked(self.next.as_ref()) };
        if unsafe { next.unbind_cpu(self.next_epoch) }.is_err() {
            panic!("prepared thread-switch rollback lost the next CPU binding");
        }
    }
}

/// Opaque previous-task binding consumed by the incoming switch tail.
#[must_use = "the incoming task must withdraw the previous CPU binding"]
#[derive(Debug)]
pub struct PreviousThreadBinding {
    previous: NonNull<CurrentThreadHeader>,
    epoch: CpuBindingEpoch,
}

impl PreviousThreadBinding {
    /// Withdraws the exact previous binding after architecture registers have
    /// switched to the incoming task.
    ///
    /// # Errors
    ///
    /// Returns [`ThreadSwitchError::PreviousThreadMismatch`] if `previous`
    /// differs from the prepared task, or
    /// [`ThreadSwitchError::StalePreviousBinding`] for an obsolete tail.
    ///
    /// # Safety
    ///
    /// The incoming switch tail must be the sole owner of this token and the
    /// previous task allocation must remain pinned and alive.
    pub unsafe fn finish(
        self,
        previous: Pin<&CurrentThreadHeader>,
    ) -> Result<(), ThreadSwitchError> {
        if previous.as_non_null() != self.previous {
            return Err(ThreadSwitchError::PreviousThreadMismatch);
        }
        unsafe { previous.unbind_cpu(self.epoch) }
    }
}

/// Validates and binds a complete scheduler thread switch transaction.
///
/// All fallible validation occurs before the returned prepared token can be
/// committed. Dropping the prepared token automatically rolls back `next`.
///
/// # Safety
///
/// The caller must own the IRQ-disabled scheduler switch path. Both headers
/// must remain pinned and alive through the raw switch and incoming tail.
pub unsafe fn prepare_thread_switch<'switch>(
    pin: &'switch CpuPin<'_>,
    previous: Pin<&CurrentThreadHeader>,
    next: Pin<&CurrentThreadHeader>,
) -> Result<(PreparedThreadSwitch<'switch>, PreviousThreadBinding), ThreadSwitchError> {
    let published = current_thread(pin).map_err(|error| match error {
        CpuLocalError::CurrentThreadMismatch => ThreadSwitchError::CurrentThreadMismatch,
        other => ThreadSwitchError::CpuLocal(other),
    })?;
    if published != previous.as_non_null() {
        return Err(ThreadSwitchError::CurrentThreadMismatch);
    }
    let previous_binding = previous
        .cpu_binding()
        .filter(|binding| binding.area == pin.area())
        .ok_or(ThreadSwitchError::CurrentThreadMismatch)?;
    let next_epoch = unsafe { next.bind_cpu(pin.area()) }?;
    Ok((
        PreparedThreadSwitch {
            next: next.as_non_null(),
            next_epoch,
            current_thread: next.as_non_null().as_ptr() as usize,
            area: pin.area(),
            _scope: PhantomData,
            _not_send_or_sync: PhantomData,
        },
        PreviousThreadBinding {
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
        CpuAreaPrefix, CpuAreaRef, CpuIndex, CurrentContext, install_bootstrap_thread,
        install_cpu_area, with_cpu_pin,
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

    fn task_header(identity: usize) -> Pin<Box<CurrentThreadHeader>> {
        Box::pin(CurrentThreadHeader::new(
            CurrentContext::from_raw(identity).unwrap(),
        ))
    }

    #[test]
    fn abandoned_prepare_rolls_back_next_binding() {
        on_fresh_modeled_cpu(|area| {
            let previous = task_header(1);
            let next = task_header(2);

            // SAFETY: the modeled CPU cannot migrate or receive interrupts.
            unsafe {
                with_cpu_pin(|pin| {
                    install_bootstrap_thread(pin, previous.as_ref()).unwrap();
                    let (prepared, _previous_binding) =
                        prepare_thread_switch(pin, previous.as_ref(), next.as_ref()).unwrap();
                    assert_eq!(current_thread(pin), Ok(previous.as_ref().as_non_null()));
                    assert_eq!(next.cpu_area(), Some(area));

                    drop(prepared);

                    assert_eq!(current_thread(pin), Ok(previous.as_ref().as_non_null()));
                    assert_eq!(next.cpu_area(), None);
                })
            }
            .unwrap();
        });
    }

    #[test]
    fn prepare_reports_the_domain_mismatch_before_binding_next() {
        on_fresh_modeled_cpu(|_| {
            let published = task_header(1);
            let wrong_previous = task_header(2);
            let next = task_header(3);

            // SAFETY: the modeled CPU cannot migrate or receive interrupts.
            unsafe {
                with_cpu_pin(|pin| {
                    install_bootstrap_thread(pin, published.as_ref()).unwrap();
                    let result = prepare_thread_switch(pin, wrong_previous.as_ref(), next.as_ref());
                    assert!(matches!(
                        result,
                        Err(ThreadSwitchError::CurrentThreadMismatch)
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
            let previous = task_header(1);
            let next = task_header(2);

            // SAFETY: this host model serializes the entire switch. Returning
            // from `commit` represents resuming after the naked switch tail.
            unsafe {
                with_cpu_pin(|pin| {
                    install_bootstrap_thread(pin, previous.as_ref()).unwrap();
                    let (prepared, previous_binding) =
                        prepare_thread_switch(pin, previous.as_ref(), next.as_ref()).unwrap();

                    assert_eq!(current_thread(pin), Ok(previous.as_ref().as_non_null()));
                    assert_eq!(previous.cpu_area(), Some(area));
                    assert_eq!(next.cpu_area(), Some(area));

                    prepared.commit();

                    assert_eq!(current_thread(pin), Ok(next.as_ref().as_non_null()));
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
            let header = task_header(1);
            // SAFETY: this fresh modeled CPU exclusively owns the header.
            let stale = unsafe { header.as_ref().bind_cpu(area) }.unwrap();
            unsafe { header.as_ref().unbind_cpu(stale) }.unwrap();
            let current = unsafe { header.as_ref().bind_cpu(area) }.unwrap();

            assert_eq!(
                unsafe { header.as_ref().unbind_cpu(stale) },
                Err(ThreadSwitchError::StalePreviousBinding)
            );
            assert_eq!(header.cpu_area(), Some(area));
            unsafe { header.as_ref().unbind_cpu(current) }.unwrap();
        });
    }
}
