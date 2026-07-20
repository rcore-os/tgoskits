//! vCPU CPU-interface lifecycle binding.

use super::{GicV3Controller, state::DeliveryRetirement};
use crate::{CpuInterfaceState, GicVcpuId, IntId, VgicError, VgicResult, backend_result};

/// Per-vCPU lifecycle handle returned by [`GicV3Controller::attach_vcpu`].
#[must_use = "dropping the binding detaches the vCPU from its Redistributor"]
pub struct GicV3VcpuBinding {
    controller: GicV3Controller,
    vcpu: GicVcpuId,
}

impl core::fmt::Debug for GicV3VcpuBinding {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("GicV3VcpuBinding")
            .field("vcpu", &self.vcpu)
            .field("spi_ownership", &self.controller.config().spi_ownership())
            .finish_non_exhaustive()
    }
}

impl Drop for GicV3VcpuBinding {
    fn drop(&mut self) {
        let mut state = self.controller.inner.state.lock();
        state.active_vcpus.remove(&self.vcpu);
        state.redistributors.remove(&self.vcpu);
    }
}

impl GicV3VcpuBinding {
    pub(super) const fn new(controller: GicV3Controller, vcpu: GicVcpuId) -> Self {
        Self { controller, vcpu }
    }

    /// Returns the attached vCPU.
    pub const fn vcpu(&self) -> GicVcpuId {
        self.vcpu
    }

    /// Restores ICH state and refills empty LRs.
    pub fn load(&self) -> VgicResult {
        let state = {
            let mut controller = self.controller.inner.state.lock();
            controller.redistributor(self.vcpu, "load CPU interface")?;
            if !controller.active_vcpus.insert(self.vcpu) {
                return Err(VgicError::ResourceConflict {
                    resource: "vCPU interrupt binding",
                    detail: alloc::format!("vCPU {} is already loaded", self.vcpu.raw()),
                });
            }
            match controller.refill_cpu_interface(self.vcpu) {
                Ok(state) => state,
                Err(error) => {
                    controller.active_vcpus.remove(&self.vcpu);
                    return Err(error);
                }
            }
        };
        if let Err(error) = backend_result(
            self.controller
                .inner
                .backend
                .load_cpu_interface(self.vcpu, &state),
        ) {
            self.controller
                .inner
                .state
                .lock()
                .active_vcpus
                .remove(&self.vcpu);
            return Err(error);
        }
        Ok(())
    }

    /// Saves ICH state after guest execution.
    pub fn save(&self) -> VgicResult {
        let mut saved = self.cpu_interface_snapshot()?;
        let save_result = backend_result(
            self.controller
                .inner
                .backend
                .save_cpu_interface(self.vcpu, &mut saved),
        );
        let merge_result = match save_result {
            Ok(()) => self
                .merge_saved_state(saved, false)
                .and_then(|retirements| self.apply_retirements(retirements)),
            Err(error) => Err(error),
        };
        self.controller
            .inner
            .state
            .lock()
            .active_vcpus
            .remove(&self.vcpu);
        merge_result
    }

    /// Harvests completed LRs, refills software pending work, and reloads ICH state.
    pub fn synchronize(&self) -> VgicResult {
        let mut saved = self.cpu_interface_snapshot()?;
        backend_result(
            self.controller
                .inner
                .backend
                .save_cpu_interface(self.vcpu, &mut saved),
        )?;
        let retirements = self.merge_saved_state(saved, true)?;
        let state = self.cpu_interface_snapshot()?;
        backend_result(
            self.controller
                .inner
                .backend
                .load_cpu_interface(self.vcpu, &state),
        )?;
        self.apply_retirements(retirements)
    }

    /// Applies one trapped guest deactivation to this vCPU's interrupt state.
    ///
    /// This operation is separate from interrupt injection: it consumes an
    /// architectural CPU-interface action and preserves whether the active
    /// delivery is software-owned or backed by an assigned physical IRQ.
    pub fn deactivate(&self, intid: IntId) -> VgicResult {
        let (retirement, state) = {
            let mut controller = self.controller.inner.state.lock();
            if !controller.active_vcpus.contains(&self.vcpu) {
                return Err(VgicError::InvalidStateTransition {
                    intid,
                    operation: "deactivate virtual interrupt",
                    detail: alloc::format!("vCPU {} is not loaded", self.vcpu.raw()),
                });
            }
            let retirement = controller.deactivate_interrupt(self.vcpu, intid)?;
            let Some(retirement) = retirement else {
                return Ok(());
            };
            let state = controller.refill_cpu_interface(self.vcpu)?;
            (retirement, state)
        };
        backend_result(
            self.controller
                .inner
                .backend
                .load_cpu_interface(self.vcpu, &state),
        )?;
        self.apply_retirements(core::iter::once(retirement))
    }

    /// Returns a snapshot useful to checked architecture adapters and tests.
    pub fn cpu_interface_snapshot(&self) -> VgicResult<CpuInterfaceState> {
        Ok(self
            .controller
            .inner
            .state
            .lock()
            .redistributor(self.vcpu, "snapshot CPU interface")?
            .cpu_interface()
            .clone())
    }

    fn merge_saved_state(
        &self,
        saved: CpuInterfaceState,
        refill: bool,
    ) -> VgicResult<alloc::vec::Vec<DeliveryRetirement>> {
        self.controller
            .inner
            .state
            .lock()
            .merge_cpu_interface(self.vcpu, saved, refill)
    }

    fn apply_retirements(
        &self,
        retirements: impl IntoIterator<Item = DeliveryRetirement>,
    ) -> VgicResult {
        let mut first_error = None;
        for retirement in retirements {
            let result = match retirement {
                DeliveryRetirement::Emulated { intid } => self
                    .controller
                    .inner
                    .backend
                    .retire_emulated_interrupt(self.vcpu, intid),
                DeliveryRetirement::Physical { binding } => self
                    .controller
                    .inner
                    .backend
                    .deactivate_physical_interrupt(self.vcpu, binding),
            };
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => backend_result(Err(error)),
            None => Ok(()),
        }
    }
}
