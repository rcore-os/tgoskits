//! VM-local interrupt sink that routes device IRQ pulses into a target vCPU's
//! pending queue.
//!
//! This is the architecture-independent counterpart to direct GIC
//! list-register writes: a pulse here is recorded against the owning VM's
//! target vCPU and drained by the vCPU run loop while it is bound to a host CPU
//! (see `runtime::vcpus::inject_pending_interrupts`). RX/data-path workers that
//! run on arbitrary host CPUs may therefore safely pulse this sink; they never
//! touch virtualization system registers such as `ICH_LR_EL2` directly.

use alloc::sync::Weak;

use axdevice_base::{IrqError, IrqLineId, IrqResult, IrqSink};

use crate::AxVM;

/// Interrupt sink that queues edge pulses for a specific VM and prepare
/// generation.
///
/// The sink holds a [`Weak<AxVM>`] (not a strong reference) so device and worker
/// graphs retaining this sink cannot keep a destroyed VM alive. It is stamped
/// with the [`AxVM::prepare_generation`] at creation; a pulse is rejected once
/// the VM has been re-prepared, so a stale worker can never inject into a newer
/// generation of the same VM.
#[derive(Clone)]
pub struct VmQueuedIrqSink {
    vm: Weak<AxVM>,
    generation: usize,
    target_vcpu: usize,
}

impl VmQueuedIrqSink {
    /// Creates a sink targeting `target_vcpu` (vCPU 0 for a single-vCPU VM) at
    /// the given prepare `generation`.
    pub fn new(vm: Weak<AxVM>, generation: usize, target_vcpu: usize) -> Self {
        Self {
            vm,
            generation,
            target_vcpu,
        }
    }
}

impl IrqSink for VmQueuedIrqSink {
    fn set_level(&self, line: IrqLineId, _asserted: bool) -> IrqResult {
        // The first version only supports edge-triggered virtio-mmio devices.
        // Level signalling requires an interrupt-ACK deassert path that is not
        // modelled yet; rejecting level here keeps callers from silently relying
        // on level semantics that never reach the guest.
        Err(IrqError::Unsupported {
            line,
            operation: "set_level",
            detail: "level-triggered IRQs are not supported by the VM queued sink".into(),
        })
    }

    fn pulse(&self, line: IrqLineId) -> IrqResult {
        let Some(vm) = self.vm.upgrade() else {
            return Err(IrqError::InvalidLine {
                line,
                operation: "pulse",
                detail: "owning VM has been dropped".into(),
            });
        };

        // Reject pulses from a sink whose generation predates the VM's current
        // prepare generation. After stop/reset the VM is re-prepared (new
        // generation) with a fresh sink; a racing worker still holding the old
        // sink must fail loudly instead of injecting into the new generation.
        if vm.prepare_generation() != self.generation {
            return Err(IrqError::InvalidLine {
                line,
                operation: "pulse",
                detail: alloc::format!(
                    "sink generation {} is stale; VM is now at generation {}",
                    self.generation,
                    vm.prepare_generation()
                ),
            });
        }

        // Queue the SPI for the target vCPU; the bound AArch64 run loop drains it
        // and performs the actual list-register write. `queue_interrupt` is a
        // no-op (returning BadState) when the VM is not Running/Paused.
        crate::runtime::vcpus::queue_interrupt(vm.id(), self.target_vcpu, line.0).map_err(|err| {
            IrqError::Backend {
                line,
                operation: "pulse",
                detail: alloc::format!("{err:?}"),
            }
        })
    }
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use super::*;

    /// A sink whose owning VM has been dropped must reject pulses instead of
    /// dereferencing stale state or touching global VM lookup.
    #[test]
    fn dropped_vm_rejects_pulse() {
        let sink = VmQueuedIrqSink::new(Weak::<AxVM>::new(), 1, 0);
        let result = sink.pulse(IrqLineId(65));
        assert!(matches!(result, Err(IrqError::InvalidLine { .. })));
        assert!(matches!(
            sink.set_level(IrqLineId(65), true),
            Err(IrqError::Unsupported { .. })
        ));
    }
}
