// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! VM-wide virtual interrupt routing and delivery.
//!
//! Interrupt controllers and emulated devices should resolve architecture
//! routing first, then submit an [`InterruptRoute`] here. This keeps device
//! models from directly mutating a vCPU backend that may be running on another
//! physical CPU, or may only exist behind a host-control vCPU fd.

use ax_errno::{AxError, AxResult};
use vm_interrupt::{InterruptLineLevel, InterruptTriggerMode};

use crate::vmm::{self, VCpuRef, VMRef};

#[cfg(target_arch = "riscv64")]
const RISCV_SUPERVISOR_EXTERNAL_INTERRUPT: usize = 9;

pub(crate) use vm_interrupt::{InterruptRoute, VcpuInterruptTarget, VirtualInterrupt};

/// Convert the historical `axvisor-api::vmm::inject_interrupt` vector into a
/// routed interrupt event.
///
/// RISC-V currently uses vector 0 as the legacy VSEIP deassert encoding. The
/// shared model carries explicit line levels, so this compatibility shim is
/// kept at the axvisor-api boundary instead of leaking into device models.
#[cfg(target_arch = "riscv64")]
pub(crate) const fn interrupt_from_api_vector(vector: usize) -> VirtualInterrupt {
    if vector == 0 {
        VirtualInterrupt::deassert(RISCV_SUPERVISOR_EXTERNAL_INTERRUPT)
    } else {
        VirtualInterrupt::edge(vector)
    }
}

/// Convert the historical `axvisor-api::vmm::inject_interrupt` vector into a
/// routed interrupt event.
#[cfg(not(target_arch = "riscv64"))]
pub(crate) const fn interrupt_from_api_vector(vector: usize) -> VirtualInterrupt {
    VirtualInterrupt::edge(vector)
}

/// Deliver one virtual interrupt to every vCPU selected by `target`.
pub(crate) fn deliver_targeted_interrupt(
    vm: &VMRef,
    target: VcpuInterruptTarget,
    interrupt: VirtualInterrupt,
) -> AxResult {
    if let VcpuInterruptTarget::Vcpu(vcpu_id) = target {
        return deliver_vcpu_interrupt(vm, vcpu_id, interrupt);
    }

    for vcpu_id in 0..vm.vcpu_num() {
        if target_matches(target, vm, vcpu_id) {
            deliver_vcpu_interrupt(vm, vcpu_id, interrupt)?;
        }
    }

    Ok(())
}

/// Deliver one virtual interrupt to a single VM-local vCPU ID.
pub(crate) fn deliver_vcpu_interrupt(
    vm: &VMRef,
    vcpu_id: usize,
    interrupt: VirtualInterrupt,
) -> AxResult {
    vm.vcpu(vcpu_id).ok_or(AxError::InvalidInput)?;
    deliver_interrupt(InterruptRoute::new(vm.id(), vcpu_id, interrupt));
    Ok(())
}

fn target_matches(target: VcpuInterruptTarget, vm: &VMRef, vcpu_id: usize) -> bool {
    match target {
        VcpuInterruptTarget::Vcpu(target_vcpu_id) => vcpu_id == target_vcpu_id,
        VcpuInterruptTarget::GuestCpu(target_guest_cpu_id) => {
            vmm::vcpus::guest_cpu_id_for_vcpu(vm, vcpu_id) == target_guest_cpu_id
        }
        VcpuInterruptTarget::All {
            current_vcpu_id,
            include_current,
        } => include_current || vcpu_id != current_vcpu_id,
        VcpuInterruptTarget::GuestCpuMask { mask, base } => {
            if base == usize::MAX {
                return true;
            }
            let guest_cpu_id = vmm::vcpus::guest_cpu_id_for_vcpu(vm, vcpu_id);
            guest_cpu_id
                .checked_sub(base)
                .filter(|bit| *bit < usize::BITS as usize)
                .is_some_and(|bit| (mask & (1usize << bit)) != 0)
        }
    }
}

/// Deliver a routed interrupt through the unified VMM path.
///
/// If the target vCPU is the current bound vCPU, the interrupt is injected
/// immediately. Otherwise it is queued to the static vCPU task model, falling
/// back to the KVM host-control vCPU fd model when the VM is control-owned.
pub(crate) fn deliver_interrupt(route: InterruptRoute) {
    if let Some(context) = crate::context::try_current_vcpu_context()
        && context.vm_id == route.vm_id
        && context.vcpu_id == route.vcpu_id
        && let Some(()) = vmm::with_vm_and_vcpu(route.vm_id, route.vcpu_id, move |_, vcpu| {
            inject_current_vcpu(route, &vcpu);
        })
    {
        return;
    }

    if vmm::vcpus::queue_vcpu_interrupt(route).is_ok() {
        return;
    }

    if !queue_control_interrupt(route) {
        #[cfg(not(feature = "control"))]
        warn!(
            "Failed to queue interrupt {:?} to VM[{}] VCpu[{}]: VM vCPU resources not found",
            route.interrupt, route.vm_id, route.vcpu_id
        );

        #[cfg(not(any(target_arch = "riscv64", target_arch = "x86_64")))]
        let _ = vmm::with_vm_and_vcpu_on_pcpu(route.vm_id, route.vcpu_id, move |_, vcpu| {
            inject_virtual_interrupt(route.interrupt, &vcpu).unwrap();
        });
    }
}

#[cfg(feature = "control")]
fn queue_control_interrupt(route: InterruptRoute) -> bool {
    match crate::kvm::queue_control_vcpu_interrupt(route) {
        Ok(()) => true,
        Err(err) => {
            warn!(
                "Failed to queue interrupt {:?} to VM[{}] VCpu[{}]: {err:?}",
                route.interrupt, route.vm_id, route.vcpu_id
            );
            false
        }
    }
}

#[cfg(not(feature = "control"))]
fn queue_control_interrupt(_route: InterruptRoute) -> bool {
    false
}

fn inject_current_vcpu(route: InterruptRoute, vcpu: &VCpuRef) {
    if let Err(err) = inject_virtual_interrupt(route.interrupt, vcpu) {
        warn!(
            "Failed to inject interrupt {:?} to VM[{}] VCpu[{}]: {err:?}",
            route.interrupt, route.vm_id, route.vcpu_id
        );
    } else {
        #[cfg(target_arch = "riscv64")]
        if let Err(err) = apply_virtual_interrupt_to_bound_hart(route.interrupt, vcpu) {
            warn!(
                "Failed to apply interrupt {:?} to bound VM[{}] VCpu[{}]: {err:?}",
                route.interrupt, route.vm_id, route.vcpu_id
            );
        }
    }
}

pub(crate) fn inject_virtual_interrupt(interrupt: VirtualInterrupt, vcpu: &VCpuRef) -> AxResult {
    match interrupt.level {
        InterruptLineLevel::Assert => {
            vcpu.inject_interrupt_with_trigger(interrupt.vector, ax_trigger_mode(interrupt.trigger))
        }
        InterruptLineLevel::Deassert => {
            #[cfg(target_arch = "riscv64")]
            {
                // The current RISC-V backend encodes VSEIP deassertion as
                // vector 0. Keeping the translation here prevents generic
                // device code from depending on that backend detail.
                let _ = interrupt.vector;
                vcpu.inject_interrupt(0)
            }
            #[cfg(not(target_arch = "riscv64"))]
            {
                let _ = vcpu;
                Ok(())
            }
        }
    }
}

fn ax_trigger_mode(trigger: InterruptTriggerMode) -> axvcpu::InterruptTriggerMode {
    match trigger {
        InterruptTriggerMode::EdgeTriggered => axvcpu::InterruptTriggerMode::EdgeTriggered,
        InterruptTriggerMode::LevelTriggered => axvcpu::InterruptTriggerMode::LevelTriggered,
    }
}

#[cfg(target_arch = "riscv64")]
fn apply_virtual_interrupt_to_bound_hart(interrupt: VirtualInterrupt, vcpu: &VCpuRef) -> AxResult {
    match interrupt.level {
        InterruptLineLevel::Assert => vcpu
            .get_arch_vcpu()
            .apply_interrupt_to_bound_hart(interrupt.vector),
        InterruptLineLevel::Deassert => vcpu.get_arch_vcpu().apply_interrupt_to_bound_hart(0),
    }
}
