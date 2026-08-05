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

fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing contract start `{start}`"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing contract end `{end}`"))
        .0
}

#[test]
fn emulated_device_spi_uses_the_vm_local_vgic_without_a_host_ipi() {
    let factory = include_str!("../../axdevice/src/factory.rs");
    let resolve = between(
        factory,
        "pub fn resolve_irq(",
        "/// Builds all capabilities",
    );
    assert!(resolve.contains("interrupt_controller"));
    assert!(resolve.contains(".wired_input"));
    assert!(resolve.contains(".connect()"));
    assert!(!resolve.contains("send_ipi"));

    let core = include_str!("../../arm_vgic/src/core.rs");
    let wired_sink = between(
        core,
        "impl WiredIrqSink for VgicWiredSink",
        "struct VgicMessageSink",
    );
    assert!(wired_sink.contains("self.controller"));
    assert!(wired_sink.contains(".set_spi_level"));
    assert!(wired_sink.contains(".pulse_spi"));
    assert!(!wired_sink.contains("send_ipi"));

    let vgic = include_str!("../src/arch/aarch64/vgic.rs");
    let wake = between(
        vgic,
        "impl GicV3VcpuWake for Aarch64VcpuWake",
        "struct Aarch64VgicFactory",
    );
    assert!(wake.contains("self.kick"));
    assert!(wake.contains(".publish_from_irq(self.vcpu_id)"));
    assert!(!wake.contains("send_ipi"));
}

#[test]
fn passthrough_spi_preallocation_covers_every_selected_physical_device() {
    let parser = include_str!("../src/boot/fdt/core/parser.rs");
    let interrupts = between(
        parser,
        "pub fn parse_vm_interrupt(",
        "pub fn update_provided_fdt(",
    );
    assert!(interrupts.contains("find_all_passthrough_devices"));
    assert!(interrupts.contains("for interrupt in view.interrupts()"));
    assert!(interrupts.contains("vm_cfg.add_pass_through_irq"));

    let vgic = include_str!("../src/arch/aarch64/vgic.rs");
    let construction = between(
        vgic,
        "pub(crate) fn register_device_factories(",
        "fn assigned_spis(",
    );
    assert!(construction.contains("config.pass_through_irqs().to_vec()"));
    assert!(construction.contains("let assigned_spis = assigned_spis(&passthrough_irqs)"));
    assert!(construction.contains("with_assigned_spis(assigned_spis)"));

    let routes = include_str!("../src/arch/aarch64/gic/physical.rs");
    let registration = between(
        routes,
        "fn register(controller: &Arc<VgicCore>)",
        "/// Stops accepting new activations",
    );
    assert!(registration.contains(".config()"));
    assert!(registration.contains(".assigned_spis()"));
    assert!(registration.contains("collect::<Vec<_>>()"));
    assert!(registration.contains(".into_boxed_slice()"));
    assert!(registration.contains("AssignedSpiRouteRegistration::install(binding)"));
}

#[test]
fn arm_world_switch_and_vgic_binding_drive_delivery_and_reclamation() {
    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    let host_ops = between(
        adapter,
        "impl ArmHostOps for AxvmArmHostOps",
        "pub(crate) struct AxvmArmVcpu",
    );
    assert!(host_ops.contains("fn finish_pending_host_irq"));
    assert!(host_ops.contains("fn handle_current_host_irq"));
    assert!(host_ops.contains("gic::route_acknowledged_host_irq(token)"));

    let run = between(adapter, "fn run(&mut self)", "fn bind(&mut self)");
    let load = run.find("binding.load()").unwrap();
    let guest = run.find("self.inner.run(&host_irq_guard)").unwrap();
    let save = run.find("binding.save()").unwrap();
    assert!(load < guest && guest < save);

    let backend = include_str!("../src/arch/aarch64/gic.rs");
    let complete = between(
        backend,
        "fn complete_physical_interrupt(",
        "fn deactivate_physical_interrupt(",
    );
    assert!(complete.contains("physical::complete_assigned_spi"));
    assert!(complete.contains("cpu_interface::deactivate_spi(intid)"));
}

#[test]
fn wfi_wait_observes_canonical_vgic_state_before_and_after_timer_arming() {
    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    let wait = between(adapter, "fn wait_for_vcpu_event(", "fn vgic_runtime(");
    assert!(wait.contains("runtime.wait_until(||"));
    assert!(wait.matches("has_pending_interrupt()").count() >= 2);
    let first_check = wait.find("has_pending_interrupt()").unwrap();
    let arm_timer = wait.find("arm_timer_wait()").unwrap();
    let recheck = wait.rfind("has_pending_interrupt()").unwrap();
    assert!(first_check < arm_timer && arm_timer < recheck);

    let controller = include_str!("../../arm_vgic/src/controller/mod.rs");
    let pending = controller
        .split_once("pub fn has_pending_interrupt(&self")
        .expect("VGIC must expose canonical pending-delivery inspection")
        .1;
    assert!(pending.contains("redistributor(vcpu, \"query pending interrupt\")"));
    assert!(pending.contains("has_pending_delivery()"));
}

#[test]
fn physical_spi_hardware_policy_stays_inside_the_aarch64_boundary() {
    let vm = include_str!("../src/vm/mod.rs");
    assert!(
        !vm.contains("target_arch"),
        "architecture-neutral VM lifecycle code must not select a target architecture"
    );

    let boundary = include_str!("../../axdevice_base/src/interrupt/controller.rs");
    assert!(boundary.contains("pub trait VirtualInterruptController"));
    assert!(boundary.contains("fn wired_input("));

    let vgic = include_str!("../src/arch/aarch64/vgic.rs");
    let assigned = vgic
        .split_once("fn assigned_spis(")
        .expect("AArch64 must translate physical IRQ routes")
        .1;
    assert!(assigned.contains("AssignedSpiConfig::new"));
    assert!(assigned.contains("HostIrqId::new"));

    let physical = include_str!("../src/arch/aarch64/gic/physical.rs");
    assert!(physical.contains("static ASSIGNED_SPI_ROUTES"));
    assert!(physical.contains("controller.forward_physical_spi(self.irq)"));
    assert!(physical.contains("fn complete_assigned_spi("));

    let backend = include_str!("../src/arch/aarch64/gic.rs");
    assert!(backend.contains("cpu_interface::deactivate_spi(intid)"));
}
