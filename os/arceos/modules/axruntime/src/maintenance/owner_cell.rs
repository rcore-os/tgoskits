//! CPU-local owner/IRQ access to one pinned device state object.

use alloc::{boxed::Box, sync::Arc};
use core::{
    cell::{Cell, UnsafeCell},
    marker::{PhantomData, PhantomPinned},
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};

use thiserror::Error;

use super::{
    MaintenanceClosed, MaintenanceError, MaintenanceLifecycle, MaintenanceRegistrar,
    MaintenanceSession, MaintenanceState,
};
use crate::task::ThreadId;

/// Local owner-cell registration or access error.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LocalOwnerCellError {
    /// The registrar no longer runs as the CPU-pinned maintenance owner.
    #[error(transparent)]
    OwnerValidation(#[from] MaintenanceError),
    /// This stable storage was already assigned to a maintenance domain.
    #[error("local owner cell is already bound to a maintenance domain")]
    AlreadyBound,
    /// The registrar and owner control belong to different domains.
    #[error("local owner control belongs to another maintenance domain")]
    ForeignDomain,
    /// The maintenance lifecycle no longer permits this access.
    #[error("local owner cell access is not allowed in state {0:?}")]
    InvalidState(MaintenanceState),
    /// Task-context access was attempted from hard IRQ context.
    #[error("local owner control access requires task context")]
    HardIrqContext,
    /// IRQ access was attempted outside hard IRQ context.
    #[error("local owner IRQ access requires hard IRQ context")]
    NotHardIrq,
    /// Access came from a CPU other than the registered owner.
    #[error("local owner cell belongs to CPU {expected}, observed CPU {actual}")]
    WrongCpu {
        /// Registered owner CPU.
        expected: usize,
        /// Observed CPU.
        actual: usize,
    },
    /// Task-context access came from a non-owner thread.
    #[error("local owner cell control access came from a non-owner thread")]
    WrongThread,
    /// A reentrant or overlapping access violated local exclusion.
    #[error("local owner cell is already borrowed")]
    Busy,
}

/// Why a pinned owner cell could not be reclaimed.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LocalOwnerReclaimError {
    /// The close proof belongs to another maintenance domain.
    #[error("local owner close proof belongs to another domain")]
    ForeignDomain,
    /// The supplied control capability belongs to another cell.
    #[error("local owner control capability belongs to another cell")]
    ForeignControl,
    /// An IRQ capability still retains the cell.
    #[error("local owner cell still has IRQ capability references")]
    IrqCapabilityLive,
}

/// Failed reclaim with ownership preserved for a later retry.
pub struct LocalOwnerReclaimFailure<T: Send + 'static> {
    error: LocalOwnerReclaimError,
    cell: Pin<Box<LocalOwnerCell<T>>>,
    control: LocalOwnerControl<T>,
}

impl<T: Send + 'static> LocalOwnerReclaimFailure<T> {
    /// Returns the reason without consuming retained ownership.
    pub const fn error(&self) -> LocalOwnerReclaimError {
        self.error
    }

    /// Recovers the pinned cell and its owner control capability.
    pub fn into_parts(self) -> (Pin<Box<LocalOwnerCell<T>>>, LocalOwnerControl<T>) {
        (self.cell, self.control)
    }
}

/// Pinned storage accessed only by one owner thread and its same-CPU IRQ.
///
/// The access protocol uses local IRQ exclusion plus an atomic reentrancy gate;
/// it does not take a cross-CPU spin lock. The private Arc is lifetime storage,
/// not a shared mutation mechanism: this non-Send owner anchor prevents the
/// device value from being freed by an IRQ capability's final drop.
pub struct LocalOwnerCell<T: Send + 'static> {
    inner: Arc<LocalOwnerInner<T>>,
    _not_send: PhantomData<*mut ()>,
    _pin: PhantomPinned,
}

impl<T: Send + 'static> LocalOwnerCell<T> {
    /// Allocates and pins one owner-local value during device initialization.
    pub fn pin(value: T) -> Pin<Box<Self>> {
        Box::pin(Self {
            inner: Arc::new(LocalOwnerInner::new(value)),
            _not_send: PhantomData,
            _pin: PhantomPinned,
        })
    }

    /// Reclaims the value after the matching session reached Closed.
    pub fn reclaim(
        self: Pin<Box<Self>>,
        control: LocalOwnerControl<T>,
        closed: &MaintenanceClosed,
    ) -> Result<T, LocalOwnerReclaimFailure<T>> {
        let domain = self.inner.domain.load(Ordering::Acquire);
        if domain.is_null() || !ptr::eq(domain, Arc::as_ptr(&closed.lifecycle)) {
            return Err(LocalOwnerReclaimFailure {
                error: LocalOwnerReclaimError::ForeignDomain,
                cell: self,
                control,
            });
        }
        if !Arc::ptr_eq(&self.inner, &control.inner) {
            return Err(LocalOwnerReclaimFailure {
                error: LocalOwnerReclaimError::ForeignControl,
                cell: self,
                control,
            });
        }
        if Arc::strong_count(&self.inner) != 2 {
            return Err(LocalOwnerReclaimFailure {
                error: LocalOwnerReclaimError::IrqCapabilityLive,
                cell: self,
                control,
            });
        }

        drop(control);
        let domain_lease = unsafe {
            // SAFETY: Closed proves all IRQ capabilities are gone, and the
            // consumed control plus this owner are the only remaining cell
            // references. Clear the binding lease before unwrapping storage.
            (*self.inner.domain_lease.get()).take()
        };
        drop(domain_lease);
        let cell = unsafe {
            // SAFETY: reclaim owns the pinned Box after every IRQ capability
            // is gone. The API never exposed Pin<&mut T>, so moving T now does
            // not violate a pinning guarantee.
            Pin::into_inner_unchecked(self)
        };
        let LocalOwnerCell { inner, .. } = *cell;
        let inner = Arc::try_unwrap(inner)
            .unwrap_or_else(|_| panic!("closed local owner cell retained an untracked capability"));
        Ok(inner.value.into_inner())
    }
}

struct LocalOwnerInner<T: Send + 'static> {
    value: UnsafeCell<T>,
    borrowed: AtomicBool,
    domain: AtomicPtr<MaintenanceLifecycle>,
    domain_lease: UnsafeCell<Option<Arc<MaintenanceLifecycle>>>,
}

impl<T: Send + 'static> LocalOwnerInner<T> {
    fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
            borrowed: AtomicBool::new(false),
            domain: AtomicPtr::new(ptr::null_mut()),
            domain_lease: UnsafeCell::new(None),
        }
    }

    fn bind(&self, lifecycle: &Arc<MaintenanceLifecycle>) -> Result<(), LocalOwnerCellError> {
        self.domain
            .compare_exchange(
                ptr::null_mut(),
                Arc::as_ptr(lifecycle).cast_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| LocalOwnerCellError::AlreadyBound)?;
        unsafe {
            // SAFETY: the successful one-time CAS is the unique initialization
            // right, and the cell is not yet visible to an IRQ capability.
            *self.domain_lease.get() = Some(Arc::clone(lifecycle));
        }
        Ok(())
    }

    fn try_borrow(&self) -> Result<LocalBorrow<'_, T>, LocalOwnerCellError> {
        self.borrowed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| LocalOwnerCellError::Busy)?;
        Ok(LocalBorrow { inner: self })
    }
}

// SAFETY: every access is serialized by the atomic borrow gate, and safe
// capabilities additionally enforce one CPU plus local IRQ exclusion.
unsafe impl<T: Send + 'static> Sync for LocalOwnerInner<T> {}

struct LocalBorrow<'cell, T: Send + 'static> {
    inner: &'cell LocalOwnerInner<T>,
}

impl<T: Send + 'static> LocalBorrow<'_, T> {
    fn value(&mut self) -> &mut T {
        unsafe {
            // SAFETY: construction exclusively changed borrowed false->true,
            // and this guard releases it only after the returned borrow ends.
            &mut *self.inner.value.get()
        }
    }
}

impl<T: Send + 'static> Drop for LocalBorrow<'_, T> {
    fn drop(&mut self) {
        self.inner.borrowed.store(false, Ordering::Release);
    }
}

/// `!Send` owner-thread control capability.
pub struct LocalOwnerControl<T: Send + 'static> {
    inner: Arc<LocalOwnerInner<T>>,
    lifecycle: Arc<MaintenanceLifecycle>,
    owner_cpu: usize,
    owner_thread: ThreadId,
    _not_send: PhantomData<*mut ()>,
}

impl<T: Send + 'static> LocalOwnerControl<T> {
    /// Mints another move-only IRQ capability from the matching registrar.
    ///
    /// Each registered handler receives an independent capability and close
    /// accounting reference. The unique owner control is never duplicated.
    pub fn mint_additional_irq<E: Copy + Send + 'static>(
        &self,
        registrar: &MaintenanceRegistrar<E>,
    ) -> Result<LocalOwnerIrq<T>, LocalOwnerCellError> {
        registrar.local_owner_irq(self)
    }

    /// Runs one bounded owner operation with local IRQs disabled.
    pub fn with_owner<R>(
        &self,
        operation: impl FnOnce(&mut T) -> R,
    ) -> Result<R, LocalOwnerCellError> {
        if !self.lifecycle.permits_control_access() {
            return Err(LocalOwnerCellError::InvalidState(self.lifecycle.state()));
        }
        let irq = self.enter_owner_context()?;
        let mut borrow = self.inner.try_borrow()?;
        let result = operation(borrow.value());
        drop(borrow);
        drop(irq);
        Ok(result)
    }

    fn enter_owner_context(&self) -> Result<ax_kspin::IrqGuard, LocalOwnerCellError> {
        if ax_hal::irq::in_irq_context() {
            return Err(LocalOwnerCellError::HardIrqContext);
        }
        let irq = ax_kspin::IrqGuard::new();
        let actual_cpu = ax_hal::percpu::this_cpu_id_pinned(irq.cpu_pin());
        if actual_cpu != self.owner_cpu {
            return Err(LocalOwnerCellError::WrongCpu {
                expected: self.owner_cpu,
                actual: actual_cpu,
            });
        }
        if crate::task::current_thread_id().map_err(|_| LocalOwnerCellError::WrongThread)?
            != self.owner_thread
        {
            return Err(LocalOwnerCellError::WrongThread);
        }
        Ok(irq)
    }
}

/// Move-only same-CPU hard-IRQ access capability.
///
/// It is `Send` so a registrar can move it into the registered callback, but
/// it is not `Sync` and cannot be cloned or used outside the owner CPU's hard
/// IRQ context.
pub struct LocalOwnerIrq<T: Send + 'static> {
    inner: Arc<LocalOwnerInner<T>>,
    lifecycle: Arc<MaintenanceLifecycle>,
    owner_cpu: usize,
    _not_sync: PhantomData<Cell<()>>,
}

impl<T: Send + 'static> LocalOwnerIrq<T> {
    /// Runs one bounded IRQ operation against the owner-local device state.
    pub fn with_irq<R>(
        &mut self,
        operation: impl FnOnce(&mut T) -> R,
    ) -> Result<R, LocalOwnerCellError> {
        if !ax_hal::irq::in_irq_context() {
            return Err(LocalOwnerCellError::NotHardIrq);
        }
        if !self.lifecycle.permits_irq_access() {
            return Err(LocalOwnerCellError::InvalidState(self.lifecycle.state()));
        }
        let actual_cpu = ax_hal::percpu::this_cpu_id();
        if actual_cpu != self.owner_cpu {
            return Err(LocalOwnerCellError::WrongCpu {
                expected: self.owner_cpu,
                actual: actual_cpu,
            });
        }
        let mut borrow = self.inner.try_borrow()?;
        let result = operation(borrow.value());
        drop(borrow);
        Ok(result)
    }
}

impl<T: Send + 'static> Drop for LocalOwnerIrq<T> {
    fn drop(&mut self) {
        self.lifecycle.release_irq_capability();
    }
}

impl<E: Copy + Send + 'static> MaintenanceRegistrar<E> {
    /// Mints paired owner and IRQ capabilities for one pinned local value.
    pub fn local_owner_cell<T: Send + 'static>(
        &self,
        cell: Pin<&LocalOwnerCell<T>>,
    ) -> Result<(LocalOwnerControl<T>, LocalOwnerIrq<T>), LocalOwnerCellError> {
        self.validate_owner()?;
        self.core()
            .lifecycle
            .register_irq_capability()
            .map_err(|_| LocalOwnerCellError::InvalidState(self.core().lifecycle.state()))?;
        if let Err(error) = cell.inner.bind(&self.core().lifecycle) {
            self.core().lifecycle.release_irq_capability();
            return Err(error);
        }
        Ok((
            LocalOwnerControl {
                inner: Arc::clone(&cell.inner),
                lifecycle: Arc::clone(&self.core().lifecycle),
                owner_cpu: self.owner_cpu(),
                owner_thread: self.owner_thread(),
                _not_send: PhantomData,
            },
            LocalOwnerIrq {
                inner: Arc::clone(&cell.inner),
                lifecycle: Arc::clone(&self.core().lifecycle),
                owner_cpu: self.owner_cpu(),
                _not_sync: PhantomData,
            },
        ))
    }

    /// Mints an additional handler capability for an existing local owner cell.
    pub fn local_owner_irq<T: Send + 'static>(
        &self,
        control: &LocalOwnerControl<T>,
    ) -> Result<LocalOwnerIrq<T>, LocalOwnerCellError> {
        self.validate_owner()?;
        mint_local_owner_irq(
            control,
            &self.core().lifecycle,
            self.owner_cpu(),
            LocalIrqRegistration::Registering,
        )
    }
}

impl<E: Copy + Send + 'static> MaintenanceSession<E> {
    /// Mints replacement IRQ access for a matching local owner cell.
    ///
    /// The prior endpoint must first be disabled, synchronized, and dropped.
    /// This operation is restricted to the live session's pinned owner thread;
    /// the returned capability participates in the same close accounting.
    pub fn local_owner_irq<T: Send + 'static>(
        &self,
        control: &LocalOwnerControl<T>,
    ) -> Result<LocalOwnerIrq<T>, LocalOwnerCellError> {
        if !Arc::ptr_eq(&control.lifecycle, self.lifecycle()) {
            return Err(LocalOwnerCellError::ForeignDomain);
        }
        if self.state() != MaintenanceState::Live {
            return Err(LocalOwnerCellError::InvalidState(self.state()));
        }
        let irq = control.enter_owner_context()?;
        let capability = mint_local_owner_irq(
            control,
            self.lifecycle(),
            self.owner_cpu(),
            LocalIrqRegistration::Live,
        );
        drop(irq);
        capability
    }
}

#[derive(Clone, Copy)]
enum LocalIrqRegistration {
    Registering,
    Live,
}

fn mint_local_owner_irq<T: Send + 'static>(
    control: &LocalOwnerControl<T>,
    lifecycle: &Arc<MaintenanceLifecycle>,
    owner_cpu: usize,
    registration: LocalIrqRegistration,
) -> Result<LocalOwnerIrq<T>, LocalOwnerCellError> {
    let bound = control.inner.domain.load(Ordering::Acquire);
    if !Arc::ptr_eq(&control.lifecycle, lifecycle)
        || bound.is_null()
        || !ptr::eq(bound, Arc::as_ptr(lifecycle))
    {
        return Err(LocalOwnerCellError::ForeignDomain);
    }
    match registration {
        LocalIrqRegistration::Registering => lifecycle.register_irq_capability(),
        LocalIrqRegistration::Live => lifecycle.register_live_irq_capability(),
    }
    .map_err(|_| LocalOwnerCellError::InvalidState(lifecycle.state()))?;
    Ok(LocalOwnerIrq {
        inner: Arc::clone(&control.inner),
        lifecycle: Arc::clone(lifecycle),
        owner_cpu,
        _not_sync: PhantomData,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_and_reclaim_wait_for_every_irq_handler_capability() {
        let lifecycle = Arc::new(MaintenanceLifecycle::new());
        let cell = LocalOwnerCell::pin(41_u32);
        cell.inner.bind(&lifecycle).unwrap();
        lifecycle.register_irq_capability().unwrap();
        let control = LocalOwnerControl {
            inner: Arc::clone(&cell.inner),
            lifecycle: Arc::clone(&lifecycle),
            owner_cpu: 0,
            owner_thread: ThreadId::from_parts(7, 3),
            _not_send: PhantomData,
        };
        let first_irq = LocalOwnerIrq {
            inner: Arc::clone(&cell.inner),
            lifecycle: Arc::clone(&lifecycle),
            owner_cpu: 0,
            _not_sync: PhantomData,
        };
        let second_irq =
            mint_local_owner_irq(&control, &lifecycle, 0, LocalIrqRegistration::Registering)
                .unwrap();

        lifecycle.activate().unwrap();
        let rebound_irq =
            mint_local_owner_irq(&control, &lifecycle, 0, LocalIrqRegistration::Live).unwrap();
        lifecycle.begin_close().unwrap();
        drop(first_irq);
        assert_eq!(
            lifecycle.try_begin_draining(),
            Err(super::super::MaintenanceLifecycleError::IrqCapabilitiesLive(2))
        );
        drop(second_irq);
        assert_eq!(
            lifecycle.try_begin_draining(),
            Err(super::super::MaintenanceLifecycleError::IrqCapabilitiesLive(1))
        );
        drop(rebound_irq);
        lifecycle.try_begin_draining().unwrap();
        lifecycle.finish_close(false).unwrap();

        let closed = MaintenanceClosed {
            lifecycle,
            _not_send: PhantomData,
        };
        let value = match cell.reclaim(control, &closed) {
            Ok(value) => value,
            Err(failure) => panic!("closed cell reclaim failed: {}", failure.error()),
        };
        assert_eq!(value, 41);
    }
}
