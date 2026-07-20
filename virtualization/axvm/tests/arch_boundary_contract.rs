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
fn vm_core_does_not_handle_arch_local_exits() {
    let vm_rs = include_str!("../src/vm/mod.rs");

    for forbidden in [
        "ArchOps",
        "CurrentArch::handle_vcpu_exit",
        "VcpuRunAction",
        "HostInterrupt",
    ] {
        assert!(
            !vm_rs.contains(forbidden),
            "vm/mod.rs must not contain architecture-local exit handling detail: {forbidden}"
        );
    }
}

#[test]
fn common_vm_initialization_only_uses_high_level_arch_entrypoints() {
    let vm = include_str!("../src/vm/mod.rs");
    let preparation = include_str!("../src/vm/prepare.rs");
    let common_sources = [
        vm,
        preparation,
        include_str!("../src/vm/prepare/address_space.rs"),
        include_str!("../src/vm/prepare/devices.rs"),
        include_str!("../src/vm/prepare/vcpus.rs"),
    ];

    assert!(vm.contains("CurrentArch::create_vm_resources(config)"));
    assert!(preparation.contains("CurrentArch::init_vm"));

    for source in &common_sources {
        for line in source.lines().filter(|line| line.contains("CurrentArch::")) {
            assert!(
                line.contains("CurrentArch::create_vm_resources")
                    || line.contains("CurrentArch::init_vm"),
                "common VM initialization calls a fine-grained architecture hook: {line}"
            );
        }
    }

    for forbidden in [
        "configure_interrupt_fabric",
        "register_arch_devices",
        "append_arch_owned_regions",
        "map_arch_address_space",
        "new_vcpu_create_state",
        "build_vcpu_create_config",
        "build_vcpu_setup_config",
    ] {
        assert!(
            common_sources
                .iter()
                .all(|source| !source.contains(forbidden)),
            "common VM initialization must not call architecture step hook: {forbidden}"
        );
    }
}

#[test]
fn every_architecture_owns_vm_resource_creation_and_initialization() {
    for source in [
        include_str!("../src/arch/aarch64/vm.rs"),
        include_str!("../src/arch/loongarch64/vm.rs"),
        include_str!("../src/arch/riscv64/vm.rs"),
        include_str!("../src/arch/x86_64/vm.rs"),
    ] {
        assert!(source.contains("fn create_vm_resources"));
        assert!(source.contains("fn init_vm"));
    }
}

#[test]
fn machine_plan_owns_interrupt_topology_construction() {
    let preparation = include_str!("../src/vm/prepare.rs");
    assert!(!preparation.contains("prepare_with_topology"));
    assert!(!preparation.contains("VmInitRequest::Provided"));
    assert!(!preparation.contains("DeviceFactory"));

    for source in [
        include_str!("../src/arch/aarch64/vm.rs"),
        include_str!("../src/arch/loongarch64/vm.rs"),
        include_str!("../src/arch/riscv64/vm.rs"),
        include_str!("../src/arch/x86_64/vm.rs"),
    ] {
        assert!(!source.contains("VmInitRequest::Provided"));
        assert!(source.contains("InterruptTopology::new(vm.interrupt_delivery())"));
        assert!(source.contains("let (interrupt_topology, interrupt_authority)"));
        assert!(source.contains("interrupt_authority)"));
    }
}

#[test]
fn riscv_host_plic_notifications_are_edge_adapted_before_topology() {
    let source = include_str!("../src/arch/riscv64/irq.rs");
    let connection = source
        .split_once("fn connect_external_irq_line(")
        .expect("RISC-V must connect planned host IRQ sources")
        .1
        .split_once("fn signal_external_interrupt")
        .expect("RISC-V host IRQ connection must precede signaling")
        .0;

    assert!(connection.contains("InterruptTriggerMode::EdgeTriggered"));
    assert!(!connection.contains("interrupt.trigger()"));
}

#[test]
fn failed_vm_initialization_resets_transient_resources_before_retry() {
    let preparation = include_str!("../src/vm/prepare.rs");
    let initialize = preparation
        .split_once("let prepared = match initialize")
        .expect("VM initialization must handle architecture errors")
        .1;

    assert!(initialize.contains("resources.reset_transient_resources()"));
    assert!(initialize.contains("return Err(err)"));
}

#[test]
fn runtime_vcpu_loop_only_consumes_scheduler_actions() {
    let runtime_vcpus_rs = include_str!("../src/runtime/vcpus.rs");

    for forbidden in [
        "VcpuRunAction::Continue",
        "VcpuRunAction::HostInterrupt",
        "HostInterrupt",
    ] {
        assert!(
            !runtime_vcpus_rs.contains(forbidden),
            "runtime/vcpus.rs must not match architecture-local exit action: {forbidden}"
        );
    }
}

#[test]
fn controller_state_is_harvested_before_vcpu_exit_side_effects() {
    let ops = include_str!("../src/architecture/ops.rs");
    let run_loop = ops
        .split_once("fn run_vcpu(")
        .expect("architecture operations must define the vCPU run loop")
        .1
        .split_once("pub(crate) fn target_phys_cpu_ids")
        .expect("the vCPU run loop must precede affinity helpers")
        .0;
    let after_run = run_loop
        .split_once("let exit = vcpu.run()?;")
        .expect("the vCPU run loop must enter the architecture backend")
        .1;
    let handle_exit = after_run
        .find("Self::handle_vcpu_exit_bound")
        .expect("the vCPU run loop must apply architecture exit side effects");
    let synchronize_controller = after_run
        .find("interrupt_topology.synchronize_vcpu")
        .expect("the vCPU run loop must synchronize interrupt controllers after exit");

    assert!(
        synchronize_controller < handle_exit,
        "hardware LR state must be folded into the controller before a guest MMIO/sysreg exit \
         observes or modifies interrupt state"
    );
}

#[test]
fn aarch64_direct_delivery_keeps_the_virtual_cpu_interface_loaded() {
    let ops = include_str!("../src/architecture/ops.rs");
    let run_loop = ops
        .split_once("fn run_vcpu(")
        .expect("architecture operations must define the vCPU run loop")
        .1
        .split_once("pub(crate) fn target_phys_cpu_ids")
        .expect("the vCPU run loop must precede affinity helpers")
        .0;
    assert!(
        run_loop.contains("Self::with_vcpu_interrupt_context(vm, ||"),
        "the architecture must keep ICH load, guest exits, and ICH save in one interrupt context"
    );

    let setup = include_str!("../src/arch/aarch64/vm.rs");
    assert!(setup.contains("Ok(ArmVcpuSetupConfig)"));
    assert!(
        !run_loop.contains("IrqSave"),
        "direct delivery must not mask all host IRQs while a guest runs"
    );

    let arm_vcpu = include_str!("../../arm_vcpu/src/vcpu.rs");
    let run = arm_vcpu
        .split_once("pub fn run(&mut self)")
        .expect("arm_vcpu must define its guest run entry")
        .1
        .split_once("/// Binds this vCPU")
        .expect("the guest run entry must precede vCPU binding")
        .0;
    assert!(run.contains("host_daif"));
    assert!(run.contains("msr daif, {host_daif}"));
    assert!(
        !run.contains("msr daifclr"),
        "arm_vcpu must restore the caller's DAIF state instead of enabling host IRQs \
         unconditionally"
    );
}

#[test]
fn aarch64_cpu_interface_switch_is_one_irq_atomic_transaction() {
    let arch = include_str!("../src/arch/aarch64/mod.rs");
    let interrupt_context = arch
        .split_once("fn with_vcpu_interrupt_context<T>")
        .expect("AArch64 must define its ICH ownership critical section")
        .1
        .split_once("fn after_external_interrupt")
        .expect("the ICH ownership critical section must precede IRQ dispatch")
        .0;
    assert!(
        interrupt_context.contains("IrqSave::new()"),
        "ICH load, synchronize, and unload must not be interrupted by a host IRQ"
    );

    let cpu_interface = include_str!("../src/arch/aarch64/gic/cpu_interface.rs");
    let load = cpu_interface
        .split_once("pub(super) fn load(")
        .expect("the AArch64 GIC backend must load ICH state")
        .1
        .split_once("pub(super) fn save(")
        .expect("ICH load must precede ICH save")
        .0;
    let disable = load
        .find("ICH_HCR_EL2.set(0)")
        .expect("ICH must remain disabled while a vCPU state is restored");
    let restore_lrs = load
        .find("write_list_register(index, entry)")
        .expect("ICH load must restore list registers");
    let enable = load
        .rfind("ICH_HCR_EL2.set(state.hcr())")
        .expect("ICH load must restore the saved control state last");
    assert!(
        disable < restore_lrs && restore_lrs < enable,
        "ICH load must disable HCR, restore all state, then enable HCR"
    );
    assert!(
        load[restore_lrs..enable].contains("data_sync_barrier()"),
        "restored LR state must reach the GIC before ICH is enabled"
    );

    let save = cpu_interface
        .split_once("pub(super) fn save(")
        .expect("the AArch64 GIC backend must save ICH state")
        .1
        .split_once("fn disable_cpu_interface")
        .expect("ICH save must precede hardware-state release")
        .0;
    let barrier = save
        .find("data_sync_barrier()")
        .expect("ICH save must synchronize guest GIC state before reading LRs");
    let read_lr = save
        .find("read_list_register(index, *slot)")
        .expect("ICH save must harvest list registers");
    assert!(barrier < read_lr);
}

#[test]
fn aarch64_lr_overflow_has_a_host_maintenance_irq_path() {
    let gic = include_str!("../src/arch/aarch64/gic/mod.rs");
    assert!(gic.contains("mod maintenance;"));
    let arch = include_str!("../src/arch/aarch64/mod.rs");
    assert!(
        arch.contains("maintenance_interrupt: Option<gic::HostMaintenanceInterrupt>"),
        "the VM architecture state must own the maintenance IRQ registration"
    );

    let roles = include_str!("../src/arch/aarch64/gic/roles.rs");
    assert!(
        roles.contains("pub(crate) const fn maintenance_interrupt(&self) -> PpiId"),
        "maintenance PPI discovery must be exposed as a checked internal capability"
    );

    let vm = include_str!("../src/arch/aarch64/vm.rs");
    assert!(vm.contains("register_maintenance_interrupt("));
    assert!(vm.contains("set_gic_controller("));
}

#[test]
fn aarch64_internal_exit_keeps_interrupt_context_loaded() {
    let source = include_str!("../src/arch/aarch64/mod.rs");

    assert!(
        source.contains("ArmVmExit::Nothing => Ok(BoundVcpuExit::Continue)"),
        "an exit handled entirely inside arm_vcpu must resume within the current run slice"
    );
}

#[test]
fn production_sources_keep_architecture_cfg_inside_arch_module() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let violations = find_target_arch_cfg_outside_arch(&source_root, &source_root);

    assert!(
        violations.is_empty(),
        "target_arch must stay inside src/arch; found: {}",
        violations.join(", ")
    );
}

#[test]
fn arch_root_contains_only_architecture_directories_and_dispatch_page() {
    let arch_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/arch");
    let mut unexpected_entries = std::fs::read_dir(&arch_root)
        .expect("AxVM architecture directory must be readable")
        .map(|entry| entry.expect("AxVM architecture entry must be readable"))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            !matches!(
                name.as_str(),
                "aarch64" | "loongarch64" | "riscv64" | "x86_64" | "mod.rs"
            )
        })
        .collect::<Vec<_>>();
    unexpected_entries.sort();

    assert!(
        unexpected_entries.is_empty(),
        "arch root must contain only architecture directories and the dispatch page; found: {}",
        unexpected_entries.join(", ")
    );
}

#[test]
fn arch_dispatch_page_does_not_own_common_implementations() {
    let dispatch = include_str!("../src/arch/mod.rs");

    for forbidden in [
        "#[path",
        "trait ArchOps",
        "struct MmioReadExit",
        "fn handle_mmio_read",
        "fn default_vcpu_affinities",
    ] {
        assert!(
            !dispatch.contains(forbidden),
            "arch/mod.rs must only select and export the current architecture: {forbidden}"
        );
    }
}

#[test]
fn common_domains_live_outside_architecture_directories() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    for relative_path in [
        "boot/fdt/mod.rs",
        "boot/images/mod.rs",
        "host/arceos.rs",
        "npt.rs",
    ] {
        assert!(
            source_root.join(relative_path).is_file(),
            "common AxVM domain must use its canonical source path: {relative_path}"
        );
    }
}

#[test]
fn axvisor_enables_synchronous_cross_cpu_irq_operations() {
    let manifest = include_str!("../../../os/axvisor/Cargo.toml");
    let ax_std = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("ax-std ="))
        .expect("Axvisor must depend on ax-std");

    assert!(
        ax_std.contains("\"ipi\""),
        "Axvisor configures per-CPU interrupt state and therefore needs ax-std's full IPI \
         capability"
    );
}

#[test]
fn vm_domain_uses_a_canonical_directory_module() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    assert!(
        source_root.join("vm/mod.rs").is_file(),
        "the VM domain with child modules must use vm/mod.rs as its directory page"
    );
    assert!(
        !source_root.join("vm.rs").exists(),
        "vm.rs must not coexist with the vm child-module directory"
    );
}

#[test]
fn common_modules_do_not_include_architecture_sources() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    find_source_files(&source_root, &mut |path, source| {
        if !path.starts_with(source_root.join("arch"))
            && source.contains("#[path")
            && source.contains("arch/")
        {
            violations.push(source_relative_path(&source_root, path));
        }
    });

    assert!(
        violations.is_empty(),
        "common modules must not include implementations from src/arch: {}",
        violations.join(", ")
    );
}

#[test]
fn architecture_directories_only_select_their_own_target() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let arch_root = source_root.join("arch");
    let architectures = ["aarch64", "loongarch64", "riscv64", "x86_64"];
    let mut violations = Vec::new();

    for architecture in architectures {
        find_source_files(&arch_root.join(architecture), &mut |path, source| {
            for other_architecture in architectures {
                if other_architecture != architecture
                    && source.contains(&format!("target_arch = \"{other_architecture}\""))
                {
                    violations.push(format!(
                        "{} selects {other_architecture}",
                        source_relative_path(&source_root, path)
                    ));
                }
            }
        });
    }

    assert!(
        violations.is_empty(),
        "an architecture directory must not select another target: {}",
        violations.join(", ")
    );
}

#[test]
fn axvisor_vm_creation_uses_unified_guest_boot_facade() {
    let axvisor_config =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../os/axvisor/src/config.rs");
    let source = std::fs::read_to_string(&axvisor_config)
        .expect("Axvisor VM creation source must be readable");

    for legacy_call in [
        "handle_fdt_operations",
        "ImageLoader::new",
        "x86_linux_direct_boot_config",
        "DEFAULT_X86_BIOS_LOAD_GPA",
    ] {
        assert!(
            !source.contains(legacy_call),
            "Axvisor VM creation must use the unified AxVM boot facade: {legacy_call}"
        );
    }
}

#[test]
fn host_time_trait_only_exposes_common_clock_capabilities() {
    let host_traits = include_str!("../src/host/traits.rs");

    for architecture_specific_detail in ["CancelToken", "fn register_timer"] {
        assert!(
            !host_traits.contains(architecture_specific_detail),
            "HostTime must not expose architecture-specific timer details: \
             {architecture_specific_detail}"
        );
    }
}

#[test]
fn aarch64_timer_serializes_generation_check_with_token_install() {
    let source = include_str!("../src/arch/aarch64/timer/state.rs");
    let schedule = source
        .split_once("fn schedule(")
        .expect("AArch64 timer must have a scheduling step")
        .1
        .split_once("fn cancel_scheduled(")
        .expect("AArch64 timer scheduling must precede cancellation")
        .0;
    let lock = schedule
        .find("self.scheduled_token.lock()")
        .expect("timer scheduling must lock its token slot");
    let generation = schedule
        .find("self.generation.load(Ordering::Acquire)")
        .expect("timer scheduling must reject stale generations");
    let install = schedule
        .find("replace(token)")
        .expect("timer scheduling must install the new token");

    assert!(
        lock < generation && generation < install,
        "the generation check and token replacement must share one serialized section"
    );
}

#[test]
fn vcpu_setup_context_keeps_named_capabilities() {
    let types = include_str!("../src/architecture/types.rs");
    let ops = include_str!("../src/architecture/ops.rs");
    let preparation = include_str!("../src/vm/prepare/vcpus.rs");

    assert!(
        [&types, &ops, &preparation]
            .into_iter()
            .all(|source| !source.contains("VcpuSetupContext")),
        "vCPU setup must pass named configuration and memory sources without a union context"
    );
}

#[test]
fn vm_init_capability_traits_are_not_reintroduced() {
    let capabilities = include_str!("../src/architecture/capabilities.rs");
    let ops = include_str!("../src/architecture/ops.rs");

    for forbidden in [
        "trait DevicePlatform",
        "trait AddressSpacePlatform",
        "VcpuCreateContext",
        "fn build_vcpu_create_config",
        "fn build_vcpu_setup_config",
    ] {
        assert!(
            !capabilities.contains(forbidden) && !ops.contains(forbidden),
            "VM initialization detail must stay behind CurrentArch::init_vm: {forbidden}"
        );
    }
}

#[test]
fn eager_vm_lifecycle_has_no_uninit_state() {
    let status = include_str!("../src/lifecycle/status.rs");
    let machine = include_str!("../src/lifecycle/machine.rs");
    let vm = include_str!("../src/vm/mod.rs");

    assert!(!status.contains("Uninit"));
    assert!(!machine.contains("Machine::Uninit"));
    assert!(vm.contains("machine: Mutex::new(Machine::Ready(resources))"));
}

#[test]
fn shared_vcpu_protocol_does_not_expose_interrupt_controller_operations() {
    let source = include_str!("../../axvm-types/src/lib.rs");
    let vcpu_protocol = source
        .split_once("pub trait VmArchVcpuOps")
        .expect("axvm-types must define the shared vCPU protocol")
        .1
        .split_once("pub trait VmArchPerCpuOps")
        .expect("the vCPU protocol must precede the per-CPU protocol")
        .0;

    for controller_operation in ["fn inject_interrupt", "fn handle_eoi"] {
        assert!(
            !vcpu_protocol.contains(controller_operation),
            "interrupt controller operation leaked into the shared vCPU protocol: \
             {controller_operation}"
        );
    }
}

#[test]
fn aarch64_passthrough_irq_binding_defers_hardware_handoff_until_activation() {
    let source = include_str!("../src/arch/aarch64/gic/physical_spi.rs");
    let bind = source
        .split_once("pub(super) fn bind(")
        .expect("AArch64 passthrough must define physical IRQ binding")
        .1
        .split_once("pub(super) fn prepare_enabled(")
        .expect("physical IRQ binding must precede activation")
        .0;

    assert!(bind.contains("reserve_irq("));
    for premature_handoff in ["host_irq::set_enable", "host_irq::set_affinity"] {
        assert!(
            !bind.contains(premature_handoff),
            "binding must preserve host IRQ delivery until guest activation: {premature_handoff}"
        );
    }

    let activation = source
        .split_once("pub(super) fn prepare_enabled(")
        .expect("AArch64 passthrough must define physical IRQ activation")
        .1
        .split_once("pub(super) fn unbind(")
        .expect("physical IRQ activation must precede unbinding")
        .0;
    assert!(activation.contains("claim_irq_for_guest("));
    assert!(activation.contains("host_irq::set_affinity"));
    assert!(
        !activation.contains("host_irq::set_enable"),
        "physical ownership preparation must not bypass the registered forwarding action"
    );

    let forwarding = include_str!("../src/arch/aarch64/gic/forwarding.rs");
    let direct_state = forwarding
        .split_once("pub(super) fn set_direct_enabled(")
        .expect("direct forwarding must expose a checked enable transition")
        .1
        .split_once("pub(super) fn retire_guest_interrupt(")
        .expect("direct enable must precede mediated retirement")
        .0;
    assert!(direct_state.contains("host_irq::enable_irq(registration)"));
    assert!(direct_state.contains("host_irq::disable_irq(registration)"));
}

#[test]
fn aarch64_host_console_stays_owned_by_the_hypervisor() {
    let source = include_str!("../src/arch/aarch64/fdt.rs");
    let snapshot = source
        .split_once("pub fn current_host_platform_snapshot()")
        .expect("AArch64 must capture the live host platform")
        .1
        .split_once("fn fdt_generation")
        .expect("host snapshot construction must precede generation hashing")
        .0;

    assert!(snapshot.contains("grant_whole_machine_assignment"));
    assert!(
        !snapshot.contains("grant_console_transfer"),
        "the active Axvisor console can back a virtual UART, but its MMIO and IRQ must never be \
         transferred to the guest"
    );
}

#[test]
fn aarch64_mediated_host_irq_preserves_level_line_lifetime() {
    let forwarding = include_str!("../src/arch/aarch64/gic/forwarding.rs");

    assert!(forwarding.contains("InterruptTriggerMode::LevelTriggered => line.raise()"));
    assert!(forwarding.contains("InterruptTriggerMode::EdgeTriggered => line.pulse()"));
    let lower = forwarding
        .find("line.lower()")
        .expect("level forwarding must deassert the VM-local line on guest retirement");
    let unmask = forwarding
        .find("self.unmask_host_irq()")
        .expect("guest retirement must re-enable the physical host IRQ");
    assert!(
        lower < unmask,
        "the VM-local level must clear before the host IRQ is unmasked"
    );
}

#[test]
fn aarch64_direct_host_irq_uses_an_exclusive_hardware_backed_lr() {
    let forwarding = include_str!("../src/arch/aarch64/gic/forwarding.rs");
    assert!(forwarding.contains("ShareMode::Exclusive"));
    assert!(forwarding.contains("controller.forward_physical_spi(self.spi)"));
    assert!(forwarding.contains("IrqReturn::Forwarded"));

    let cpu_interface = include_str!("../src/arch/aarch64/gic/cpu_interface.rs");
    assert!(cpu_interface.contains("ICH_LR_EL2::HW::SET"));
    assert!(cpu_interface.contains("ICH_LR_EL2::PINTID"));

    let arm_vcpu = include_str!("../../arm_vcpu/src/vcpu.rs");
    assert!(arm_vcpu.contains("HCR_EL2::IMO::EnableVirtualIRQ"));
    assert!(arm_vcpu.contains("HCR_EL2::FMO::EnableVirtualFIQ"));
}

#[test]
fn aarch64_direct_delivery_has_no_physical_private_interrupt_backend() {
    let vgic_backend = include_str!("../../arm_vgic/src/backend.rs");
    for obsolete_operation in [
        "load_physical_private_interrupts",
        "save_physical_private_interrupts",
        "synchronize_physical_private_interrupts",
        "update_physical_private_interrupts",
        "send_physical_sgi",
    ] {
        assert!(
            !vgic_backend.contains(obsolete_operation),
            "direct delivery must keep SGIs and PPIs virtual instead of exposing the obsolete \
             physical-private backend operation {obsolete_operation}"
        );
    }

    let axvm_backend = include_str!("../src/arch/aarch64/gic/mod.rs");
    assert!(!axvm_backend.contains("mod private_interrupts;"));
    assert!(!axvm_backend.contains("PrivateInterruptMask"));
    assert!(!axvm_backend.contains("PrivateInterruptState"));

    let vgic_config = include_str!("../../arm_vgic/src/config.rs");
    assert!(!vgic_config.contains("passthrough_private_interrupts"));

    let arm_vcpu = include_str!("../../arm_vcpu/src/vcpu.rs");
    assert!(!arm_vcpu.contains("passthrough_interrupt"));
    assert!(!arm_vcpu.contains("passthrough_timer"));
}

#[test]
fn aarch64_cpu_interface_save_relinquishes_hardware_state() {
    let cpu_interface = include_str!("../src/arch/aarch64/gic/cpu_interface.rs");
    let save = cpu_interface
        .split_once("pub(super) fn save(")
        .expect("AArch64 GIC backend must save its CPU interface")
        .1
        .split_once("pub(super) fn hardware_list_register_count")
        .expect("CPU-interface save must precede hardware capability helpers")
        .0;
    let save_body = save
        .split_once("fn disable_cpu_interface()")
        .expect("CPU-interface save must have a hardware relinquish step")
        .0;

    let read_lr = save_body
        .find("read_list_register(index, *slot)?")
        .expect("CPU-interface save must harvest every guest LR");
    let relinquish = save_body
        .find("disable_cpu_interface();")
        .expect("CPU-interface save must always relinquish hardware state");
    let clear_lr = save
        .find("ich_lr_el2_set(index, LocalRegisterCopy::new(0))")
        .expect("CPU-interface save must invalidate every hardware LR");
    let disable = save
        .find("ICH_HCR_EL2.set(0)")
        .expect("CPU-interface save must disable the virtual CPU interface");

    assert!(
        read_lr < relinquish && clear_lr < disable,
        "saved LRs must be harvested, invalidated, and followed by disabling ICH_HCR_EL2"
    );
}

#[test]
fn aarch64_emulated_timer_progresses_while_the_vcpu_is_not_running() {
    let capabilities = include_str!("../src/arch/aarch64/capabilities.rs");

    assert!(
        capabilities.contains(
            "const VM_TIMER_INTEGRATION: VmTimerIntegration = VmTimerIntegration::RuntimeCallback;"
        ),
        "AArch64 VM timer expiry must run from the host timer callback even after WFI yields the \
         vCPU task"
    );
}

#[test]
fn aarch64_passthrough_routes_separate_mpidr_from_host_cpu_index() {
    let registration = include_str!("../src/arch/aarch64/gic/registration.rs");
    let placement = include_str!("../src/arch/aarch64/placement.rs");
    let vm = include_str!("../src/arch/aarch64/vm.rs");

    assert!(
        !registration.contains("VcpuRoute::new(placement.id, placement.phys_cpu_id, affinity)"),
        "the guest MPIDR affinity must not be reused as an AxVM logical CPU index"
    );
    assert!(
        registration.contains("placement.fixed_host_cpu()?"),
        "passthrough routing must consume the normalized fixed CPU mask"
    );
    assert!(placement.contains("super::capabilities::logical_cpu_id"));
    assert!(placement.contains("config.phys_cpu_ls.set_guest_cpu_sets(cpu_sets)"));
    assert!(placement.contains("mask & available_cpu_mask != mask"));
    assert!(placement.contains("mask.count_ones() != 1"));
    assert!(placement.contains("mask.trailing_zeros() as usize"));
    let normalize = vm
        .find("normalize_direct_vcpu_cpu_sets(&mut config)?")
        .expect("direct vCPU placement must be normalized during VM resource creation");
    let consume = vm
        .find("let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids()")
        .expect("VM resource creation must consume normalized vCPU placements");
    assert!(normalize < consume);
}

fn find_target_arch_cfg_outside_arch(
    source_root: &std::path::Path,
    directory: &std::path::Path,
) -> Vec<String> {
    let mut violations = Vec::new();
    for entry in std::fs::read_dir(directory).expect("AxVM source directory must be readable") {
        let entry = entry.expect("AxVM source directory entry must be readable");
        let path = entry.path();
        if path.is_dir() {
            if path != source_root.join("arch") {
                violations.extend(find_target_arch_cfg_outside_arch(source_root, &path));
            }
            continue;
        }

        if path.extension().is_some_and(|extension| extension == "rs")
            && std::fs::read_to_string(&path)
                .expect("AxVM source file must be readable")
                .contains("target_arch")
        {
            violations.push(
                path.strip_prefix(source_root)
                    .expect("source path must be below src")
                    .display()
                    .to_string(),
            );
        }
    }
    violations
}

fn find_source_files(directory: &std::path::Path, visit: &mut impl FnMut(&std::path::Path, &str)) {
    for entry in std::fs::read_dir(directory).expect("AxVM source directory must be readable") {
        let entry = entry.expect("AxVM source directory entry must be readable");
        let path = entry.path();
        if path.is_dir() {
            find_source_files(&path, visit);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let source = std::fs::read_to_string(&path).expect("AxVM source file must be readable");
            visit(&path, &source);
        }
    }
}

fn source_relative_path(source_root: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(source_root)
        .expect("source path must be below src")
        .display()
        .to_string()
}
