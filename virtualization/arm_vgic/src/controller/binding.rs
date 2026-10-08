//! vCPU CPU-interface lifecycle binding.

use alloc::vec::Vec;

use ax_sync::RawSpinLockIrqSaveGuard;

use super::{ControllerState, CpuInterfacePhase, GicV3Native, state::DeliveryRetirement};
use crate::{CpuInterfaceState, GicVcpuId, IntId, VgicError, VgicResult, backend_result};

/// Upper bound on the retirements one CPU-interface merge can decode beyond the
/// list-register sweep.
///
/// The virtual EOI count is the five-bit `ICH_HCR_EL2.EOIcount` field, and each
/// of those EOIs retires at most one active delivery.
const MAX_VIRTUAL_EOI_RETIREMENTS: usize = 31;

/// Per-vCPU lifecycle handle returned by `attach_vcpu`.
#[must_use = "dropping the binding detaches the vCPU from its Redistributor"]
pub struct GicV3VcpuBinding {
    controller: GicV3Native,
    vcpu: GicVcpuId,
    /// Reserved capacity for one bound of decoded retirements.
    ///
    /// Computed once at attach time from immutable configuration, so the loaded
    /// path can reserve its output buffer before it takes the canonical raw
    /// lock and never grow or free that buffer while the lock is held. The
    /// bound is one retirement per list register, one per virtual EOI, and one
    /// for a trapped deactivation applied in the same batch.
    retirement_capacity: usize,
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
        state.redistributors.remove(&self.vcpu);
    }
}

impl GicV3VcpuBinding {
    pub(super) fn new(controller: GicV3Native, vcpu: GicVcpuId) -> Self {
        let retirement_capacity =
            controller.inner.config.list_register_count() + MAX_VIRTUAL_EOI_RETIREMENTS + 1;
        Self {
            controller,
            vcpu,
            retirement_capacity,
        }
    }

    /// Returns the attached vCPU.
    pub const fn vcpu(&self) -> GicVcpuId {
        self.vcpu
    }

    /// Reserves the bounded output buffer one merge can fill.
    ///
    /// Called before any canonical raw guard is taken so the merge itself never
    /// reaches the allocator while canonical state is locked.
    fn retirement_buffer(&self) -> Vec<DeliveryRetirement> {
        Vec::with_capacity(self.retirement_capacity)
    }

    fn controller_state(&self) -> RawSpinLockIrqSaveGuard<'_, ControllerState> {
        self.controller.inner.state.lock_irqsave()
    }

    /// Restores ICH state and refills empty LRs.
    pub fn load(&self) -> VgicResult {
        let state = {
            let mut controller = self.controller_state();
            controller.redistributor(self.vcpu, "load CPU interface")?;
            if controller.cpu_interface_phase(self.vcpu) != CpuInterfacePhase::Idle {
                return Err(VgicError::ResourceConflict {
                    resource: "vCPU interrupt binding",
                    detail: alloc::format!("vCPU {} is already loaded", self.vcpu.raw()),
                });
            }
            controller.set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Loaded);
            match controller.refill_cpu_interface(self.vcpu) {
                Ok(state) => state,
                Err(error) => {
                    let rollback = controller.rollback_cpu_interface_load(self.vcpu);
                    controller.set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Idle);
                    rollback?;
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
            let mut controller = self.controller_state();
            let rollback = controller.rollback_cpu_interface_load(self.vcpu);
            controller.set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Idle);
            rollback?;
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
        // Reserved outside the canonical guard so the merge below cannot grow
        // or free this buffer while raw state is locked.
        let mut retirements = self.retirement_buffer();
        let (merge_result, retiring) = {
            let mut controller = self.controller_state();
            let result = save_result.and_then(|()| {
                controller.merge_cpu_interface(self.vcpu, saved, false, &mut retirements)
            });
            let retiring = result.is_ok() && !retirements.is_empty();
            if retiring {
                controller.set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Retiring);
            } else {
                controller.set_cpu_interface_phase(self.vcpu, CpuInterfacePhase::Idle);
            }
            (result, retiring)
        };
        // A failed merge never reaches the retirement callbacks, matching the
        // pre-split behavior; a successful one applies the reserved batch after
        // the canonical guard is released.
        merge_result?;
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
        let mut retirements = self.retirement_buffer();
        self.merge_saved_state(saved, true, &mut retirements)?;
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
        let mut saved = {
            let controller = self.controller_state();
            if !controller.cpu_interface_loaded(self.vcpu) {
                return Err(VgicError::InvalidStateTransition {
                    intid,
                    operation: "deactivate virtual interrupt",
                    detail: alloc::format!("vCPU {} is not loaded", self.vcpu.raw()),
                });
            }
            controller
                .redistributor(self.vcpu, "deactivate virtual interrupt")?
                .cpu_interface()
                .clone()
        };

        // TDIR can exit immediately after hardware changes an LR from Pending
        // to Active. Harvest ICH state here so this architectural operation
        // never depends on an outer run loop having synchronized first.
        backend_result(
            self.controller
                .inner
                .backend
                .save_cpu_interface(self.vcpu, &mut saved),
        )?;
        let mut retirements = self.retirement_buffer();
        let state = {
            let mut controller = self.controller_state();
            controller.merge_cpu_interface(self.vcpu, saved, false, &mut retirements)?;
            if let Some(retirement) = controller.deactivate_interrupt(self.vcpu, intid)? {
                retirements.push(retirement);
            }
            controller.refill_cpu_interface(self.vcpu)?
        };
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
        let mut retirements = self.retirement_buffer();
        {
            let mut controller = self.controller_state();
            if controller.cpu_interface_phase(self.vcpu) != CpuInterfacePhase::Idle {
                return Err(VgicError::InvalidStateTransition {
                    intid,
                    operation: "deactivate saved virtual interrupt",
                    detail: alloc::format!("vCPU {} is still loaded", self.vcpu.raw()),
                });
            }
            if let Some(retirement) = controller.deactivate_interrupt(self.vcpu, intid)? {
                retirements.push(retirement);
            }
            controller.refill_cpu_interface(self.vcpu)?;
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
        retirements: &mut Vec<DeliveryRetirement>,
    ) -> VgicResult<()> {
        self.controller_state()
            .merge_cpu_interface(self.vcpu, saved, refill, retirements)
    }

    /// Runs the backend callbacks of one reserved retirement batch.
    ///
    /// Called only after every canonical raw guard is released, so device
    /// callbacks and wakes never execute under raw state.
    fn apply_retirements(&self, retirements: &[DeliveryRetirement]) -> VgicResult {
        let mut first_error = None;
        for retirement in retirements {
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
