//! vCPU CPU-interface lifecycle binding.

use super::GicV3Controller;
use crate::{CpuInterfaceState, GicVcpuId, VgicError, VgicResult, backend_result};

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
            Ok(()) => self.merge_saved_state(saved, false),
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
        self.merge_saved_state(saved, true)?;
        let state = self.cpu_interface_snapshot()?;
        backend_result(
            self.controller
                .inner
                .backend
                .load_cpu_interface(self.vcpu, &state),
        )
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

    fn merge_saved_state(&self, saved: CpuInterfaceState, refill: bool) -> VgicResult {
        let retired = self
            .controller
            .inner
            .state
            .lock()
            .merge_cpu_interface(self.vcpu, saved, refill)?;
        let mut first_error = None;
        for intid in retired {
            if let Err(error) = self
                .controller
                .inner
                .backend
                .retire_emulated_interrupt(self.vcpu, intid)
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
