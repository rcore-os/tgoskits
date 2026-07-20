//! IRQ action whose registration, control, and teardown belong to one owner.

use alloc::{boxed::Box, string::String, sync::Arc};
use core::marker::PhantomData;

use ax_hal::irq::{IrqContext, IrqId, IrqReturn};

use super::{
    MaintenanceError, MaintenanceLifecycle, MaintenanceRegistrar, MaintenanceSession,
    MaintenanceState,
};
#[cfg(feature = "block")]
use crate::irq::DetachedRegistration;
use crate::{irq::Registration, task::ThreadId};

/// One IRQ action bound to the thread and CPU that registered it.
///
/// This wrapper is deliberately `!Send`: device source control, action
/// enablement, synchronization, and removal must all execute in the same
/// maintenance domain. The hard-IRQ callback remains `Send` because the IRQ
/// framework owns it after registration, but its affinity is fixed to
/// [`Self::owner_cpu`].
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_runtime::maintenance::MaintenanceIrqAction>();
/// ```
#[derive(Debug)]
#[must_use = "the owner must explicitly close or retain this IRQ action"]
pub struct MaintenanceIrqAction {
    registration: Option<Registration>,
    lifecycle_capability: Option<MaintenanceIrqActionCapability>,
    lifecycle: Arc<MaintenanceLifecycle>,
    owner_cpu: usize,
    owner_thread: ThreadId,
    _not_send: PhantomData<*mut ()>,
}

/// Failed linear action close with the complete owner capability retained.
#[derive(Debug, thiserror::Error)]
#[error("maintenance IRQ action close failed: {reason}")]
#[must_use = "retry close or retain the complete action in owner quarantine"]
pub struct MaintenanceIrqCloseFailure {
    reason: MaintenanceError,
    registration: Box<MaintenanceIrqAction>,
}

/// Detached owner-bound callback retained while another runtime owns the IRQ.
///
/// This token remains `!Send`: only the maintenance owner that registered the
/// action may reattach or destroy its callback after guest routing is revoked.
/// Dropping it without an explicit close consumes the detached callback but
/// leaves its action capability counted and quarantines the maintenance domain,
/// so the outer runner cannot release the owner CPU lease.
#[cfg(feature = "block")]
#[must_use = "reattach or explicitly close this detached IRQ action"]
pub(crate) struct MaintenanceDetachedIrqAction {
    registration: Option<DetachedRegistration>,
    lifecycle_capability: Option<MaintenanceIrqActionCapability>,
    lifecycle: Arc<MaintenanceLifecycle>,
    owner_cpu: usize,
    owner_thread: ThreadId,
    _not_send: PhantomData<*mut ()>,
}

/// Failed detach with the complete active owner capability retained.
#[cfg(feature = "block")]
pub(crate) struct MaintenanceIrqDetachFailure {
    reason: MaintenanceError,
    action: Box<MaintenanceIrqAction>,
}

/// Failed reattach with the complete detached owner capability retained.
#[cfg(feature = "block")]
pub(crate) struct MaintenanceIrqReattachFailure {
    reason: MaintenanceError,
    action: Box<MaintenanceDetachedIrqAction>,
}

/// Failed detached-action close with the complete callback token retained.
#[cfg(feature = "block")]
pub(crate) struct MaintenanceDetachedIrqCloseFailure {
    reason: MaintenanceError,
    action: Box<MaintenanceDetachedIrqAction>,
}

/// Linear close-accounting owner for one registered or detached IRQ action.
///
/// Callback-local capabilities have independent counts because safe callers
/// may register an action that captures no [`super::LocalIrqWake`] or
/// [`super::LocalOwnerIrq`]. Dropping this owner is therefore fail-closed: it
/// quarantines the domain and deliberately leaves the count live. Only a
/// successful explicit action close releases it.
#[derive(Debug)]
struct MaintenanceIrqActionCapability {
    lifecycle: Arc<MaintenanceLifecycle>,
    live: bool,
}

#[derive(Clone, Copy)]
enum MaintenanceIrqActionPhase {
    Registering,
    Live,
}

impl MaintenanceIrqActionCapability {
    fn reserve(
        lifecycle: &Arc<MaintenanceLifecycle>,
        phase: MaintenanceIrqActionPhase,
    ) -> Result<Self, MaintenanceError> {
        match phase {
            MaintenanceIrqActionPhase::Registering => lifecycle.register_irq_capability(),
            MaintenanceIrqActionPhase::Live => lifecycle.register_live_irq_capability(),
        }?;
        Ok(Self {
            lifecycle: Arc::clone(lifecycle),
            live: true,
        })
    }

    fn release(mut self) {
        self.lifecycle.release_irq_capability();
        self.live = false;
    }
}

impl Drop for MaintenanceIrqActionCapability {
    fn drop(&mut self) {
        if self.live {
            self.lifecycle.quarantine();
        }
    }
}

impl<T: Copy + Send + 'static> MaintenanceRegistrar<T> {
    /// Registers one shared action disabled and fixed to this owner CPU.
    ///
    /// The registrar is the only API that chooses affinity: callers cannot
    /// accidentally register the callback on another CPU or fall back to an
    /// unconstrained shared action.
    pub fn register_shared_disabled(
        &self,
        name: impl Into<String>,
        irq: IrqId,
        handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
    ) -> Result<MaintenanceIrqAction, MaintenanceError> {
        self.validate_owner()?;
        register_owner_action(
            Arc::clone(&self.core().lifecycle),
            self.owner_cpu(),
            self.owner_thread(),
            MaintenanceIrqActionPhase::Registering,
            name,
            irq,
            handler,
        )
    }
}

impl<T: Copy + Send + 'static> MaintenanceSession<T> {
    /// Registers one replacement action disabled on this live owner's CPU.
    ///
    /// Replacement is needed when initialization and normal I/O use distinct
    /// portable IRQ endpoints. The live owner must first mask, synchronize,
    /// close, and release the previous endpoint capability.
    pub fn register_shared_disabled(
        &self,
        name: impl Into<String>,
        irq: IrqId,
        handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
    ) -> Result<MaintenanceIrqAction, MaintenanceError> {
        self.validate_owner()?;
        register_owner_action(
            Arc::clone(self.lifecycle()),
            self.owner_cpu(),
            self.owner_thread(),
            MaintenanceIrqActionPhase::Live,
            name,
            irq,
            handler,
        )
    }
}

fn register_owner_action(
    lifecycle: Arc<MaintenanceLifecycle>,
    owner_cpu: usize,
    owner_thread: ThreadId,
    phase: MaintenanceIrqActionPhase,
    name: impl Into<String>,
    irq: IrqId,
    handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
) -> Result<MaintenanceIrqAction, MaintenanceError> {
    let lifecycle_capability = MaintenanceIrqActionCapability::reserve(&lifecycle, phase)?;
    let handler = lifecycle_gated_irq_handler(Arc::clone(&lifecycle), handler);
    let registration =
        match Registration::register_shared_disabled_on(name, irq, owner_cpu, handler) {
            Ok(registration) => registration,
            Err(error) => {
                lifecycle_capability.release();
                return Err(MaintenanceError::from(error));
            }
        };
    Ok(MaintenanceIrqAction {
        registration: Some(registration),
        lifecycle_capability: Some(lifecycle_capability),
        lifecycle,
        owner_cpu,
        owner_thread,
        _not_send: PhantomData,
    })
}

/// Prevents a quarantined registration from retaining an arbitrary endpoint.
///
/// `Registration::drop` deliberately preserves an action whose hardware
/// teardown was not proven. The lifecycle gate therefore remains part of the
/// registered callback: the first late interrupt contains the backing line
/// without invoking device code or publishing into a closed mailbox.
fn lifecycle_gated_irq_handler(
    lifecycle: Arc<MaintenanceLifecycle>,
    mut handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
) -> impl FnMut(IrqContext) -> IrqReturn + Send + 'static {
    move |context| {
        if lifecycle.permits_irq_access() {
            handler(context)
        } else {
            IrqReturn::MaskLineAndWake
        }
    }
}

impl MaintenanceIrqAction {
    /// Returns the CPU that owns this action and receives its IRQ.
    pub const fn owner_cpu(&self) -> usize {
        self.owner_cpu
    }

    /// Returns the generation-bearing scheduler identity that owns this action.
    pub const fn owner_thread(&self) -> ThreadId {
        self.owner_thread
    }

    /// Enables this previously registered action on its owner CPU.
    pub fn enable(&self) -> Result<(), MaintenanceError> {
        self.validate_owner()?;
        if !self.lifecycle.permits_action_enable() {
            return Err(MaintenanceError::Lifecycle(
                super::MaintenanceLifecycleError::InvalidState {
                    expected: MaintenanceState::Live,
                    actual: self.lifecycle.state(),
                },
            ));
        }
        self.registration()?
            .enable()
            .map_err(MaintenanceError::from)
    }

    /// Disables this action while retaining its callback and IRQ identity.
    pub fn disable(&self) -> Result<(), MaintenanceError> {
        self.validate_owner()?;
        self.registration()?
            .disable()
            .map_err(MaintenanceError::from)
    }

    /// Acquires fail-closed containment for the complete backing line.
    ///
    /// Activation uses this only when masking the exact device source failed.
    /// The action remains disabled and retains quench ownership in quarantine;
    /// unrelated shared-line peers resume only after explicit recovery proves
    /// device-side containment and releases the quench.
    pub fn quench_line(&self) -> Result<(), MaintenanceError> {
        self.validate_owner()?;
        self.registration()?
            .quench_line()
            .map_err(MaintenanceError::from)
    }

    /// Reopens a line quenched by this action after its device source is safe.
    pub fn release_quench(&self) -> Result<(), MaintenanceError> {
        self.validate_owner()?;
        self.registration()?
            .release_quench()
            .map_err(MaintenanceError::from)
    }

    /// Waits until no hard-IRQ invocation of this action remains in flight.
    pub fn synchronize(&self) -> Result<(), MaintenanceError> {
        self.validate_owner()?;
        self.registration()?
            .synchronize()
            .map_err(MaintenanceError::from)
    }

    /// Returns the owner-local action and backing-line state.
    pub(crate) fn status(&self) -> Result<ax_hal::irq::IrqStatus, MaintenanceError> {
        self.validate_owner()?;
        self.registration()?.status().map_err(MaintenanceError::from)
    }

    /// Consumes the action after disabling, draining, and removing it.
    ///
    /// Every failure returns this complete typed registration. Losing the
    /// callback would also lose its `LocalIrqWake` and source-generation
    /// ownership, so callers must retry or park the maintenance session with
    /// the returned value still alive.
    pub fn close(mut self) -> Result<(), MaintenanceIrqCloseFailure> {
        if let Err(reason) = self.validate_owner() {
            return Err(MaintenanceIrqCloseFailure {
                reason,
                registration: Box::new(self),
            });
        }
        let registration = self
            .registration
            .take()
            .expect("maintenance IRQ action was already consumed");
        match registration.close() {
            Ok(()) => {
                let capability = self
                    .lifecycle_capability
                    .take()
                    .expect("live maintenance IRQ action owns close accounting");
                capability.release();
                Ok(())
            }
            Err(failure) => {
                let (reason, registration) = failure.into_parts();
                self.registration = Some(registration);
                Err(MaintenanceIrqCloseFailure {
                    reason: MaintenanceError::from(reason),
                    registration: Box::new(self),
                })
            }
        }
    }

    /// Removes this disabled and drained action while retaining its callback.
    #[cfg(feature = "block")]
    pub(crate) fn detach(
        mut self,
    ) -> Result<MaintenanceDetachedIrqAction, MaintenanceIrqDetachFailure> {
        if let Err(reason) = self.validate_owner() {
            return Err(MaintenanceIrqDetachFailure {
                reason,
                action: Box::new(self),
            });
        }
        let registration = self
            .registration
            .take()
            .expect("maintenance IRQ action was already consumed");
        match registration.detach() {
            Ok(registration) => Ok(MaintenanceDetachedIrqAction {
                registration: Some(registration),
                lifecycle_capability: self.lifecycle_capability.take(),
                lifecycle: Arc::clone(&self.lifecycle),
                owner_cpu: self.owner_cpu,
                owner_thread: self.owner_thread,
                _not_send: PhantomData,
            }),
            Err((reason, registration)) => {
                self.registration = Some(registration);
                Err(MaintenanceIrqDetachFailure {
                    reason: MaintenanceError::from(reason),
                    action: Box::new(self),
                })
            }
        }
    }

    fn validate_owner(&self) -> Result<(), MaintenanceError> {
        validate_action_owner(&self.lifecycle, self.owner_cpu, self.owner_thread)
    }

    fn registration(&self) -> Result<&Registration, MaintenanceError> {
        self.registration
            .as_ref()
            .ok_or(MaintenanceError::Irq(ax_hal::irq::IrqError::NotFound))
    }
}

#[cfg(feature = "block")]
impl MaintenanceDetachedIrqAction {
    /// Restores the callback under its original fixed owner affinity, disabled.
    pub(crate) fn reattach(
        mut self,
    ) -> Result<MaintenanceIrqAction, MaintenanceIrqReattachFailure> {
        if let Err(reason) = self.validate_owner() {
            return Err(MaintenanceIrqReattachFailure {
                reason,
                action: Box::new(self),
            });
        }
        let registration = self
            .registration
            .take()
            .expect("detached maintenance IRQ action was already consumed");
        match registration.reattach() {
            Ok(registration) => Ok(MaintenanceIrqAction {
                registration: Some(registration),
                lifecycle_capability: self.lifecycle_capability.take(),
                lifecycle: Arc::clone(&self.lifecycle),
                owner_cpu: self.owner_cpu,
                owner_thread: self.owner_thread,
                _not_send: PhantomData,
            }),
            Err(failure) => {
                let (reason, registration) = failure.into_parts();
                self.registration = Some(registration);
                Err(MaintenanceIrqReattachFailure {
                    reason: MaintenanceError::from(reason),
                    action: Box::new(self),
                })
            }
        }
    }

    /// Destroys a detached callback after guest routing has been revoked.
    pub(crate) fn close(mut self) -> Result<(), MaintenanceDetachedIrqCloseFailure> {
        if let Err(reason) = self.validate_owner() {
            return Err(MaintenanceDetachedIrqCloseFailure {
                reason,
                action: Box::new(self),
            });
        }
        drop(self.registration.take());
        let capability = self
            .lifecycle_capability
            .take()
            .expect("detached maintenance IRQ action owns close accounting");
        capability.release();
        Ok(())
    }

    fn validate_owner(&self) -> Result<(), MaintenanceError> {
        validate_action_owner(&self.lifecycle, self.owner_cpu, self.owner_thread)
    }
}

fn validate_action_owner(
    lifecycle: &MaintenanceLifecycle,
    owner_cpu: usize,
    owner_thread: ThreadId,
) -> Result<(), MaintenanceError> {
    super::runtime::validate_owner_identity(owner_cpu, owner_thread)?;
    if !lifecycle.permits_control_access() {
        let actual = lifecycle.state();
        return Err(MaintenanceError::Lifecycle(
            super::MaintenanceLifecycleError::InvalidState {
                expected: MaintenanceState::Live,
                actual,
            },
        ));
    }
    Ok(())
}

impl Drop for MaintenanceIrqAction {
    fn drop(&mut self) {
        if self.registration.is_some() {
            // An enabled action can still own a callback containing the local
            // wake and device capture endpoint. Close publication before the
            // underlying registration enters fail-closed quarantine, so a
            // late IRQ contains its source instead of publishing into a
            // domain whose linear action owner vanished.
            self.lifecycle.quarantine();
        }
    }
}

impl MaintenanceIrqCloseFailure {
    /// Returns the close reason without losing the retained action.
    pub const fn reason(&self) -> MaintenanceError {
        self.reason
    }

    /// Recovers the still-live owner-bound action for retry or quarantine.
    pub fn into_registration(self) -> MaintenanceIrqAction {
        *self.registration
    }

    /// Splits the failure while preserving the complete owner action.
    pub fn into_parts(self) -> (MaintenanceError, MaintenanceIrqAction) {
        (self.reason, *self.registration)
    }
}

#[cfg(feature = "block")]
impl MaintenanceIrqDetachFailure {
    pub(crate) fn into_parts(self) -> (MaintenanceError, MaintenanceIrqAction) {
        (self.reason, *self.action)
    }
}

#[cfg(feature = "block")]
impl MaintenanceIrqReattachFailure {
    pub(crate) fn into_parts(self) -> (MaintenanceError, MaintenanceDetachedIrqAction) {
        (self.reason, *self.action)
    }
}

#[cfg(feature = "block")]
impl MaintenanceDetachedIrqCloseFailure {
    pub(crate) fn into_parts(self) -> (MaintenanceError, MaintenanceDetachedIrqAction) {
        (self.reason, *self.action)
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::maintenance::MaintenanceLifecycleError;

    #[test]
    fn action_without_callback_capability_blocks_close_until_explicit_release() {
        let lifecycle = Arc::new(MaintenanceLifecycle::new());
        let capability = MaintenanceIrqActionCapability::reserve(
            &lifecycle,
            MaintenanceIrqActionPhase::Registering,
        )
        .unwrap();
        lifecycle.activate().unwrap();
        lifecycle.begin_close().unwrap();

        assert_eq!(
            lifecycle.try_begin_draining(),
            Err(MaintenanceLifecycleError::IrqCapabilitiesLive(1))
        );
        capability.release();
        lifecycle.try_begin_draining().unwrap();
        lifecycle.finish_close(false).unwrap();
    }

    #[test]
    fn quarantined_action_gate_contains_late_irq_without_calling_endpoint() {
        let lifecycle = Arc::new(MaintenanceLifecycle::new());
        lifecycle.activate().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let mut handler = lifecycle_gated_irq_handler(Arc::clone(&lifecycle), move |_| {
            handler_calls.fetch_add(1, Ordering::SeqCst);
            IrqReturn::Handled
        });
        let context = IrqContext {
            irq: IrqId::new(irq_framework::IrqDomainId(1), irq_framework::HwIrq(4)),
            cpu: irq_framework::CpuId(0),
        };

        assert_eq!(handler(context), IrqReturn::Handled);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        lifecycle.quarantine();
        assert_eq!(handler(context), IrqReturn::MaskLineAndWake);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn detach_and_reattach_transfer_one_action_capability() {
        let lifecycle = Arc::new(MaintenanceLifecycle::new());
        lifecycle.activate().unwrap();
        let active =
            MaintenanceIrqActionCapability::reserve(&lifecycle, MaintenanceIrqActionPhase::Live)
                .unwrap();
        let detached = active;
        let reattached = detached;
        lifecycle.begin_close().unwrap();

        assert_eq!(
            lifecycle.try_begin_draining(),
            Err(MaintenanceLifecycleError::IrqCapabilitiesLive(1))
        );
        reattached.release();
        lifecycle.try_begin_draining().unwrap();
    }

    #[test]
    fn implicit_action_capability_drop_quarantines_without_releasing() {
        let lifecycle = Arc::new(MaintenanceLifecycle::new());
        let capability = MaintenanceIrqActionCapability::reserve(
            &lifecycle,
            MaintenanceIrqActionPhase::Registering,
        )
        .unwrap();
        lifecycle.activate().unwrap();

        drop(capability);

        assert_eq!(lifecycle.state(), MaintenanceState::Quarantined);
        assert_eq!(
            lifecycle.try_begin_draining(),
            Err(MaintenanceLifecycleError::InvalidState {
                expected: MaintenanceState::Closing,
                actual: MaintenanceState::Quarantined,
            })
        );
    }

    #[cfg(feature = "block")]
    #[test]
    fn implicit_detached_action_drop_quarantines_the_owner_domain() {
        let lifecycle = Arc::new(MaintenanceLifecycle::new());
        let capability = MaintenanceIrqActionCapability::reserve(
            &lifecycle,
            MaintenanceIrqActionPhase::Registering,
        )
        .unwrap();
        lifecycle.activate().unwrap();
        let detached = MaintenanceDetachedIrqAction {
            registration: None,
            lifecycle_capability: Some(capability),
            lifecycle: Arc::clone(&lifecycle),
            owner_cpu: 0,
            owner_thread: ThreadId::from_parts(1, 1),
            _not_send: PhantomData,
        };

        drop(detached);

        assert_eq!(lifecycle.state(), MaintenanceState::Quarantined);
    }
}
