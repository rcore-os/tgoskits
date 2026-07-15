//! AArch64 VM-local interrupt backend.

use ax_errno::AxResult;
use axdevice_base::{IrqLineId, IrqSink};
use axvm_types::{VMId, VMInterruptMode};

use crate::irq::InterruptFabric;

struct Aarch64VmIrqSink {
    vm_id: VMId,
    target_vcpu_id: usize,
}

impl IrqSink for Aarch64VmIrqSink {
    fn set_level(&self, line: IrqLineId, asserted: bool) -> AxResult {
        if asserted {
            self.pulse(line)?;
        }
        Ok(())
    }

    fn pulse(&self, line: IrqLineId) -> AxResult {
        crate::manager::inject_interrupt(self.vm_id, self.target_vcpu_id, line.0)
    }
}

pub(crate) fn configure(vm_id: VMId, mode: VMInterruptMode) -> AxResult<InterruptFabric> {
    if mode == VMInterruptMode::NoIrq {
        return Ok(InterruptFabric::new(mode));
    }

    InterruptFabric::with_sink(
        mode,
        alloc::sync::Arc::new(Aarch64VmIrqSink {
            vm_id,
            target_vcpu_id: 0,
        }),
    )
}
