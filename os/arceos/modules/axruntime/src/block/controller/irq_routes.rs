//! Linear host IRQ-action ownership across exclusive device handoff.

use alloc::vec::Vec;
use core::mem;

use ax_hal::irq::IrqError;
use rdif_block::BlkError;

use super::{BlockController, BlockHandoffError};
use crate::irq::{DetachedRegistration, Registration};

/// Transaction-local owner of every host IRQ action for one controller.
///
/// Device-side delivery is masked before the registered actions are drained
/// and detached. Dropping an unfinished transaction publishes every remaining
/// active action and detached token back into the quarantined controller.
pub(super) struct HandoffIrqOwner<'controller> {
    controller: &'controller BlockController,
    active: Vec<Registration>,
    detached: Vec<DetachedRegistration>,
    device_masked: bool,
}

impl<'controller> HandoffIrqOwner<'controller> {
    pub(super) fn take(
        controller: &'controller BlockController,
    ) -> Result<Self, BlockHandoffError> {
        if controller.detached_registrations.lock().is_some() {
            return Err(BlockHandoffError::InvalidState(controller.name.clone()));
        }
        let active = controller
            .registrations
            .lock()
            .take()
            .ok_or_else(|| BlockHandoffError::InvalidState(controller.name.clone()))?
            .into_vec();
        if active.is_empty() {
            controller.store_irq_ownership(active, Vec::new());
            return Err(BlockHandoffError::InvalidState(controller.name.clone()));
        }
        Ok(Self {
            controller,
            detached: Vec::with_capacity(active.len()),
            active,
            device_masked: false,
        })
    }

    pub(super) fn actions(&self) -> &[Registration] {
        &self.active
    }

    pub(super) fn mask_device(&mut self) -> Result<(), BlkError> {
        self.controller.device.lock().disable_irq()?;
        self.device_masked = true;
        if !self.controller.finish_masked_source_continuations() {
            return Err(BlkError::Busy);
        }
        Ok(())
    }

    /// Removes every drained action from its descriptor into a move-only token.
    pub(super) fn detach_actions(&mut self) -> Result<(), IrqError> {
        let active = mem::take(&mut self.active);
        let mut remaining = active.into_iter();
        while let Some(registration) = remaining.next() {
            match registration.detach() {
                Ok(action) => self.detached.push(action),
                Err((error, registration)) => {
                    self.active.push(registration);
                    self.active.extend(remaining);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    pub(super) fn fail_closed(&self) {
        assert!(
            self.device_masked,
            "handoff actions cannot close before device interrupt masking"
        );
        for registration in &self.active {
            let _ = registration.disable();
        }
        self.controller.mark_offline();
    }

    /// Commits descriptor removal while retaining callbacks for guest return.
    pub(super) fn publish_detached_actions(mut self) {
        assert!(
            self.active.is_empty() && !self.detached.is_empty(),
            "guest ownership requires every host IRQ action to be detached"
        );
        self.publish_owned_routes();
    }

    fn publish_owned_routes(&mut self) {
        self.controller
            .store_irq_ownership(mem::take(&mut self.active), mem::take(&mut self.detached));
    }
}

impl Drop for HandoffIrqOwner<'_> {
    fn drop(&mut self) {
        if self.active.is_empty() && self.detached.is_empty() {
            return;
        }
        self.publish_owned_routes();
    }
}

impl BlockController {
    /// Reattaches retained host callbacks under fresh, disabled action handles.
    pub(super) fn reattach_host_actions(&self) -> Result<(), BlockHandoffError> {
        if self.registrations.lock().is_some() {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        }
        let detached = self
            .detached_registrations
            .lock()
            .take()
            .ok_or_else(|| BlockHandoffError::InvalidState(self.name.clone()))?
            .into_vec();
        if detached.is_empty() {
            self.store_irq_ownership(Vec::new(), detached);
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        }

        let mut active = Vec::with_capacity(detached.len());
        let mut remaining = detached.into_iter();
        while let Some(action) = remaining.next() {
            match action.reattach() {
                Ok(registration) => active.push(registration),
                Err(error) => {
                    let (error, action) = error.into_parts();
                    let mut retained = Vec::with_capacity(remaining.len() + 1);
                    retained.push(action);
                    retained.extend(remaining);
                    self.store_irq_ownership(active, retained);
                    return Err(error.into());
                }
            }
        }
        self.store_irq_ownership(active, Vec::new());
        Ok(())
    }

    fn store_irq_ownership(&self, active: Vec<Registration>, detached: Vec<DetachedRegistration>) {
        assert!(
            self.registrations.lock().is_none(),
            "active controller IRQ ownership was concurrently replaced"
        );
        assert!(
            self.detached_registrations.lock().is_none(),
            "detached controller IRQ ownership was concurrently replaced"
        );
        if !active.is_empty() {
            *self.registrations.lock() = Some(active.into_boxed_slice());
        }
        if !detached.is_empty() {
            *self.detached_registrations.lock() = Some(detached.into_boxed_slice());
        }
    }
}
