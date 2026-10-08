//! vCPU CPU-interface lifecycle binding.

use ax_sync::RawSpinLockIrqSaveGuard;

use super::{
    ControllerState, CpuInterfacePhase, GicV3Native,
    state::{DeliveryRetirement, LoadPathFailure, RetirementBatch},
};
use crate::{CpuInterfaceState, GicVcpuId, IntId, VgicError, VgicResult, backend_result};

/// Per-vCPU lifecycle handle returned by `attach_vcpu`.
#[must_use = "dropping the binding detaches the vCPU from its Redistributor"]
pub struct GicV3VcpuBinding {
    controller: GicV3Native,
    vcpu: GicVcpuId,
}

impl core::fmt::Debug for GicV3VcpuBinding {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("GicV3VcpuBinding")
            .field("vcpu", &self.vcpu)
            .field(
                "spi_ownership",
                &self.controller.inner.config.spi_ownership(),
            )
            .finish_non_exhaustive()
    }
}

impl Drop for GicV3VcpuBinding {
    fn drop(&mut self) {
        let mut state = self.controller_state();
        state.set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Idle);
        let retired = state.redistributors.remove(&self.vcpu);
        drop(state);
        drop(retired);
    }
}

impl GicV3VcpuBinding {
    pub(super) fn new(controller: GicV3Native, vcpu: GicVcpuId) -> Self {
        Self { controller, vcpu }
    }

    /// Returns the attached vCPU.
    pub const fn vcpu(&self) -> GicVcpuId {
        self.vcpu
    }

    fn controller_state(&self) -> RawSpinLockIrqSaveGuard<'_, ControllerState> {
        self.controller.inner.state.lock_irqsave()
    }

    /// Restores ICH state and refills empty LRs.
    pub fn load(&self) -> VgicResult {
        // The attach and conflict checks run in short guards so their errors
        // are formatted only after the canonical raw lock is released.
        let attach = {
            let controller = self.controller_state();
            controller
                .redistributor_load(self.vcpu, "load CPU interface")
                .map(|_| controller.cpu_interface_phase(self.vcpu) != CpuInterfacePhase::Idle)
        };
        let already_loaded = attach.map_err(LoadPathFailure::into_vgic_error)?;
        if already_loaded {
            return Err(VgicError::NativeState {
                operation: "load CPU interface",
                vcpu: Some(self.vcpu.raw()),
                intid: None,
                reason: "the vCPU is already loaded",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            });
        }
        let load_outcome = {
            let mut controller = self.controller_state();
            controller.set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Loaded);
            match controller.refill_cpu_interface(self.vcpu) {
                Ok(state) => Ok(state),
                Err(failure) => {
                    let rollback = controller.rollback_cpu_interface_load(self.vcpu);
                    controller.set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Idle);
                    Err((failure, rollback))
                }
            }
        };
        let state = match load_outcome {
            Ok(state) => state,
            Err((failure, rollback)) => {
                rollback.map_err(LoadPathFailure::into_vgic_error)?;
                return Err(failure.into_vgic_error());
            }
        };
        if let Err(error) = backend_result(
            self.controller
                .inner
                .backend
                .load_cpu_interface(self.vcpu, &state),
        ) {
            let rollback = {
                let mut controller = self.controller_state();
                let rollback = controller.rollback_cpu_interface_load(self.vcpu);
                controller.set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Idle);
                rollback
            };
            rollback.map_err(LoadPathFailure::into_vgic_error)?;
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
        if let Err(error) = save_result {
            // The backend save failed before the canonical guard; there is no
            // hardware image to fold back.
            self.controller_state()
                .set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Idle);
            return Err(error);
        }
        // The batch lives on the stack, so the merge below never reaches the
        // allocator while raw state is locked.
        let mut retirements = RetirementBatch::new();
        let (merge_failure, retiring) = {
            let mut controller = self.controller_state();
            let result = controller.merge_cpu_interface(self.vcpu, saved, false, &mut retirements);
            let retiring = result.is_ok() && !retirements.is_empty();
            if retiring {
                controller.set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Retiring);
            } else {
                controller.set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Idle);
            }
            (result.err(), retiring)
        };
        // A failed merge never reaches the retirement callbacks, matching the
        // pre-split behavior; a successful one applies the reserved batch after
        // the canonical guard is released.
        if let Some(failure) = merge_failure {
            return Err(failure.into_vgic_error());
        }
        let result = self.apply_retirements(&retirements);
        if retiring {
            self.controller_state()
                .set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Idle);
        }
        result
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
        let mut retirements = RetirementBatch::new();
        self.merge_saved_state(saved, true, &mut retirements)
            .map_err(LoadPathFailure::into_vgic_error)?;
        let state = self.cpu_interface_snapshot()?;
        backend_result(
            self.controller
                .inner
                .backend
                .load_cpu_interface(self.vcpu, &state),
        )?;
        self.apply_retirements(&retirements)
    }

    /// Applies one trapped guest deactivation to this vCPU's interrupt state.
    ///
    /// This operation is separate from interrupt injection: it consumes an
    /// architectural CPU-interface action and preserves whether the active
    /// delivery is software-owned or backed by an assigned physical IRQ.
    pub fn deactivate(&self, intid: IntId) -> VgicResult {
        let saved = {
            let controller = self.controller_state();
            if controller.cpu_interface_loaded(self.vcpu) {
                controller
                    .redistributor_load(self.vcpu, "deactivate virtual interrupt")
                    .map(|redistributor| redistributor.cpu_interface().clone())
            } else {
                Err(LoadPathFailure::CpuInterfaceNotLoaded {
                    vcpu: self.vcpu,
                    intid,
                    operation: "deactivate virtual interrupt",
                })
            }
        };
        let mut saved = saved.map_err(LoadPathFailure::into_vgic_error)?;

        // TDIR can exit immediately after hardware changes an LR from Pending
        // to Active. Harvest ICH state here so this architectural operation
        // never depends on an outer run loop having synchronized first.
        backend_result(
            self.controller
                .inner
                .backend
                .save_cpu_interface(self.vcpu, &mut saved),
        )?;
        let mut retirements = RetirementBatch::new();
        let result = {
            let mut controller = self.controller_state();
            match controller.merge_cpu_interface(self.vcpu, saved, false, &mut retirements) {
                Ok(()) => match controller.deactivate_interrupt(self.vcpu, intid) {
                    Ok(retirement) => {
                        if let Some(retirement) = retirement {
                            retirements.push(retirement);
                        }
                        controller.refill_cpu_interface(self.vcpu).map(|_| ())
                    }
                    Err(failure) => Err(failure),
                },
                Err(failure) => Err(failure),
            }
        };
        result.map_err(LoadPathFailure::into_vgic_error)?;
        let state = self.cpu_interface_snapshot()?;
        backend_result(
            self.controller
                .inner
                .backend
                .load_cpu_interface(self.vcpu, &state),
        )?;
        self.apply_retirements(&retirements)
    }

    /// Applies a trapped DIR after the run loop has already saved ICH state.
    pub fn deactivate_saved(&self, intid: IntId) -> VgicResult {
        let mut retirements = RetirementBatch::new();
        let failure = {
            let mut controller = self.controller_state();
            if controller.cpu_interface_phase(self.vcpu) != CpuInterfacePhase::Idle {
                Some(LoadPathFailure::CpuInterfaceStillLoaded {
                    vcpu: self.vcpu,
                    intid,
                    operation: "deactivate saved virtual interrupt",
                })
            } else {
                match controller.deactivate_interrupt(self.vcpu, intid) {
                    Ok(retirement) => {
                        if let Some(retirement) = retirement {
                            retirements.push(retirement);
                        }
                        controller.refill_cpu_interface(self.vcpu).err()
                    }
                    Err(failure) => Some(failure),
                }
            }
        };
        if let Some(failure) = failure {
            return Err(failure.into_vgic_error());
        }
        self.apply_retirements(&retirements)
    }

    /// Sends a trapped ICC_SGI1R_EL1 request from this vCPU.
    pub fn write_sgi1r(&self, value: u64) -> VgicResult {
        self.controller.write_sgi1r(self.vcpu, value)
    }

    /// Reads the common guest ICC control register from saved state.
    pub fn read_icc_control(&self) -> VgicResult<u64> {
        Ok(self
            .controller_state()
            .redistributor(self.vcpu, "read ICC_CTLR_EL1")?
            .cpu_interface()
            .icc_control())
    }

    /// Writes the common guest ICC control register in saved state.
    pub fn write_icc_control(&self, value: u64) -> VgicResult {
        self.controller_state()
            .redistributor_mut(self.vcpu, "write ICC_CTLR_EL1")?
            .cpu_interface_mut()
            .set_icc_control(value);
        Ok(())
    }

    /// Reads the guest virtual priority mask from saved state.
    pub fn read_icc_priority_mask(&self) -> VgicResult<u64> {
        Ok(u64::from(
            self.controller_state()
                .redistributor(self.vcpu, "read ICC_PMR_EL1")?
                .cpu_interface()
                .icc_priority_mask(),
        ))
    }

    /// Writes the guest virtual priority mask in saved state.
    pub fn write_icc_priority_mask(&self, value: u64) -> VgicResult {
        self.controller_state()
            .redistributor_mut(self.vcpu, "write ICC_PMR_EL1")?
            .cpu_interface_mut()
            .set_icc_priority_mask(value as u8);
        Ok(())
    }

    /// Reads the virtual running priority from saved LR state.
    pub fn read_icc_running_priority(&self) -> VgicResult<u64> {
        Ok(u64::from(
            self.controller_state()
                .redistributor(self.vcpu, "read ICC_RPR_EL1")?
                .cpu_interface()
                .icc_running_priority()
                .raw(),
        ))
    }

    /// Returns a snapshot useful to checked architecture adapters and tests.
    pub fn cpu_interface_snapshot(&self) -> VgicResult<CpuInterfaceState> {
        Ok(self
            .controller_state()
            .redistributor(self.vcpu, "snapshot CPU interface")?
            .cpu_interface()
            .clone())
    }

    /// Returns whether this vCPU has a pending delivery ready for guest entry.
    pub fn has_pending_interrupt(&self) -> VgicResult<bool> {
        self.controller.has_pending_interrupt(self.vcpu)
    }

    fn merge_saved_state(
        &self,
        saved: CpuInterfaceState,
        refill: bool,
        retirements: &mut RetirementBatch,
    ) -> Result<(), LoadPathFailure> {
        self.controller_state()
            .merge_cpu_interface(self.vcpu, saved, refill, retirements)
    }

    /// Runs the backend callbacks of one reserved retirement batch.
    ///
    /// Called only after every canonical raw guard is released, so device
    /// callbacks and wakes never execute under raw state.
    fn apply_retirements(&self, retirements: &RetirementBatch) -> VgicResult {
        let mut first_error = None;
        for retirement in retirements.iter() {
            let result = match retirement {
                DeliveryRetirement::Emulated { intid, wake } => {
                    let result = backend_result(
                        self.controller
                            .inner
                            .backend
                            .retire_emulated_interrupt(self.vcpu, *intid),
                    );
                    let wake_result = wake.as_ref().map_or(Ok(()), |wake| wake.wake());
                    result.and(wake_result)
                }
                DeliveryRetirement::Physical { binding } => {
                    self.controller.complete_physical_spi(self.vcpu, *binding)
                }
            };
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use super::*;
    use crate::{
        GicAffinity, GicV3Config, GicV3Controller, GicV3MmioRegion, GicV3SpiOwnership,
        SoftwareGicV3Backend,
    };

    struct NoopWake;

    impl crate::GicV3VcpuWake for NoopWake {
        fn wake(&self) -> VgicResult {
            Ok(())
        }
    }

    #[test]
    fn controller_state_access_requires_irq_save_guard() {
        let config = GicV3Config::new(
            GicV3SpiOwnership::AllGuestOwned,
            GicV3MmioRegion::new(0x0800_0000, 0x1_0000).unwrap(),
            GicV3MmioRegion::new(0x080a_0000, 0x2_0000).unwrap(),
            0x2_0000,
            1,
        )
        .unwrap();
        let controller = GicV3Controller::new(config, Arc::new(SoftwareGicV3Backend)).unwrap();
        let binding = controller
            .attach_vcpu(
                GicVcpuId::new(0),
                GicAffinity::new(0, 0, 0, 0),
                Arc::new(NoopWake),
            )
            .unwrap();

        let guard = binding.controller_state();
        let irq_state = ax_sync::irq_save_and_disable();
        // SAFETY: `irq_state` is restored exactly once on the same test thread.
        unsafe { ax_sync::irq_restore(irq_state) };
        drop(guard);

        assert_eq!(irq_state, 0, "controller state access left IRQs enabled");
    }
}
