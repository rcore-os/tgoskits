// Copyright 2026 The Axvisor Team
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

#[test]
fn emulated_passthrough_spi_uses_the_vm_runtime_gate_without_a_host_ipi() {
    let vm = include_str!("../src/vm/mod.rs");
    let injection = vm
        .split_once("fn inject_device_irq(&self")
        .expect("AxVM must own emulated-device IRQ delivery")
        .1
        .split_once("pub(crate) fn handle_nested_page_fault")
        .expect("nested-page-fault handling must follow device IRQ delivery")
        .0;

    assert!(
        injection.contains("try_inject_passthrough_device_irq"),
        "common device delivery must dispatch through the architecture capability"
    );
    assert!(
        !injection.contains("deliver_physical_spi"),
        "the producer must not directly pend a physical SPI"
    );

    let capabilities = include_str!("../src/architecture/capabilities.rs");
    let platform_injection = capabilities
        .split_once("pub(crate) fn try_inject_passthrough_device_irq")
        .expect("the architecture capability must own passthrough device delivery")
        .1
        .split_once("/// Guest firmware preparation")
        .expect("guest firmware capabilities must follow physical-SPI delivery")
        .0;
    assert!(
        platform_injection.contains("transition_passthrough_spi")
            && platform_injection.contains("PassthroughSpiSignalRequest"),
        "passthrough delivery must publish through the per-VM/vCPU gate"
    );
    assert!(
        !platform_injection.contains("send_ipi"),
        "passthrough publication must not inject a host IPI into the guest-owned interface"
    );

    let runtime = include_str!("../src/vm/passthrough_irq.rs");
    let signal = runtime
        .split_once("pub(crate) fn signal_passthrough_spi")
        .expect("the runtime gate must expose a passthrough signal operation")
        .1
        .split_once("pub(crate) fn prepare_guest_entry")
        .expect("guest-entry preparation must follow publication")
        .0;
    assert!(
        !signal.contains("send_ipi"),
        "passthrough publication must not inject a host IPI into the guest-owned interface"
    );
}

#[test]
fn arm_host_hooks_drive_entry_delivery_and_exit_reclamation() {
    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    let host_ops = adapter
        .split_once("impl ArmHostOps for AxvmArmHostOps")
        .expect("AxVM must implement arm_vcpu host operations")
        .1
        .split_once("pub(crate) struct AxvmArmVcpu")
        .expect("the Arm host implementation must precede the vCPU adapter")
        .0;

    assert!(host_ops.contains("fn prepare_guest_entry"));
    assert!(host_ops.contains("PassthroughInterfaceOwner::Guest"));
    assert!(host_ops.contains("fn complete_guest_exit"));
    assert!(host_ops.contains("PassthroughInterfaceOwner::Host"));
}

#[test]
fn wfi_wait_uses_generic_and_passthrough_pending_work_as_its_predicate() {
    let runtime_loop = include_str!("../src/runtime/vcpus.rs");
    let wait = runtime_loop
        .split_once("fn wait(vm:")
        .expect("the vCPU runtime must own a WFI wait helper")
        .1
        .split_once("fn wait_for")
        .expect("the lifecycle wait helper must follow the WFI wait helper")
        .0;
    assert!(wait.contains("wait_until"));
    assert!(wait.contains("has_pending_vcpu_work(vcpu_id)"));

    let vm_runtime = include_str!("../src/vm/mod.rs");
    let pending = vm_runtime
        .split_once("fn has_pending_vcpu_work")
        .expect("the VM runtime must expose a combined pending-work predicate")
        .1
        .split_once("pub(crate) fn wait_until")
        .expect("wait-queue methods must follow pending-work inspection")
        .0;
    assert!(pending.contains("pending_interrupts"));
    assert!(pending.contains("passthrough_spis.has_queued_spi(vcpu_id)"));
}

#[test]
fn physical_spi_hardware_policy_stays_inside_the_architecture_boundary() {
    let vm = include_str!("../src/vm/mod.rs");
    let capabilities = include_str!("../src/architecture/capabilities.rs");
    let aarch64 = include_str!("../src/arch/aarch64/capabilities.rs");

    assert!(
        vm.contains("mod passthrough_irq;"),
        "the architecture-neutral ownership state machine must remain a normal VM submodule"
    );
    assert!(
        vm.contains("passthrough_spis: passthrough_irq::PassthroughSpiGate"),
        "every runtime must expose one uniform pending-work contract"
    );
    assert!(
        !vm.contains("target_arch"),
        "common VM code must not select a target architecture"
    );
    assert!(
        capabilities.contains("trait PhysicalSpiPlatform"),
        "physical-SPI hardware access must cross a named capability boundary"
    );
    assert!(
        aarch64.contains("impl PhysicalSpiPlatform for Aarch64Arch")
            && aarch64.contains("vcpu_task_placement")
            && aarch64.contains("single_enabled_cpu")
            && aarch64.contains("someboot::smp::cpu_idx_to_id")
            && aarch64.contains("super::gic::with_passthrough_spi_controller"),
        "AArch64 must derive the SPI route from validated task placement and own GIC access"
    );
}
