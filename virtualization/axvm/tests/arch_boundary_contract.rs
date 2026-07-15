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
fn riscv_guest_paging_selection_returns_the_unwrapped_common_level() {
    let riscv_vm = include_str!("../src/arch/riscv64/vm.rs");

    assert!(riscv_vm.contains("let levels = levels.unwrap_or(0);"));
    assert!(riscv_vm.contains("3 | 4 => Ok(levels),"));
}

#[test]
fn custom_vm_init_inputs_cross_the_arch_boundary_unchanged() {
    let preparation = include_str!("../src/vm/prepare.rs");
    assert!(preparation.contains("VmInitRequest::Provided"));
    assert!(preparation.contains("factories,"));
    assert!(preparation.contains("interrupt_fabric,"));

    for source in [
        include_str!("../src/arch/aarch64/vm.rs"),
        include_str!("../src/arch/loongarch64/vm.rs"),
        include_str!("../src/arch/x86_64/vm.rs"),
    ] {
        assert!(source.contains("VmInitRequest::Provided"));
        assert!(source.contains("init_vm_with(vm, factories, interrupt_fabric)"));
    }
    let riscv = include_str!("../src/arch/riscv64/vm.rs");
    assert!(riscv.contains("VmInitRequest::Provided"));
    assert!(riscv.contains("init_vm_with(vm, factories, interrupt_fabric, None)"));
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
fn vcpu_backend_entry_requires_one_pinned_cpu_scope() {
    let vcpu = include_str!("../src/vcpu.rs");
    let architecture_ops = include_str!("../src/architecture/ops.rs");

    assert!(vcpu.contains("struct PinnedCpuContext<'pin>"));
    assert!(vcpu.contains("cpu_pin: &'pin CpuPin"));
    assert!(
        vcpu.contains("AtomicPtr<CurrentVcpuHeader>"),
        "CURRENT_VCPU publication must tolerate hard-IRQ re-entry without mutable aliasing",
    );
    assert!(vcpu.contains("load(Ordering::Acquire)"));
    assert!(vcpu.contains("compare_exchange("));
    assert!(vcpu.contains("impl<A: VmArchVcpuOps> Drop for CurrentVcpuScope"));
    assert!(vcpu.contains("clear_current_vcpu_header(&self.vcpu.current_header"));
    for unpinned_access in [
        "current_ptr_unchecked",
        "current_ref_raw",
        "current_ref_mut_raw",
    ] {
        assert!(
            !vcpu.contains(unpinned_access),
            "AxVM current-vCPU state must use the CpuPin-aware per-CPU API: {unpinned_access}"
        );
    }
    for operation in ["fn bind", "fn run", "fn unbind"] {
        let signature = vcpu
            .split_once(operation)
            .unwrap_or_else(|| panic!("missing AxVCpu operation: {operation}"))
            .1
            .split_once('{')
            .expect("AxVCpu operation must have a body")
            .0;
        assert!(
            signature.contains("PinnedCpuContext<'_>"),
            "{operation} must require proof that the host task is CPU-pinned"
        );
    }

    assert!(architecture_ops.contains("let preempt_guard = PreemptGuard::new()"));
    assert!(architecture_ops.contains("PinnedCpuContext::new(preempt_guard.cpu_pin())"));
    assert!(architecture_ops.contains("vcpu.enter_pinned(&pinned_cpu)"));
    assert!(architecture_ops.contains("vcpu.unbind(&pinned_cpu)"));

    for backend_call in [
        ".bind(pinned_cpu.cpu_pin())",
        ".run(pinned_cpu.cpu_pin())",
        ".unbind(pinned_cpu.cpu_pin())",
    ] {
        assert!(
            vcpu.contains(backend_call),
            "AxVCpu must carry the borrowed CpuPin into backend call {backend_call}"
        );
    }

    let pinned_runner = architecture_ops
        .split_once("fn run_vcpu_pinned")
        .expect("AxVM must isolate its pinned backend runner")
        .1
        .split_once("pub(crate) fn target_phys_cpu_ids")
        .expect("pinned backend runner must end before architecture helpers")
        .0;
    assert!(pinned_runner.contains("PreemptGuard::new()"));
    assert!(pinned_runner.contains("handle_vcpu_exit_bound"));
    let publish = pinned_runner
        .find("vcpu.enter_pinned(&pinned_cpu)")
        .expect("CURRENT_VCPU must be published in the pinned runner");
    let bind = pinned_runner
        .find("vcpu.bind(&pinned_cpu)")
        .expect("vCPU bind must use the pinned context");
    let run = pinned_runner
        .find("vcpu.run(&pinned_cpu)")
        .expect("guest entry must use the pinned context");
    let handle = pinned_runner
        .find("handle_vcpu_exit_bound")
        .expect("the bound owner must process VM exit state before IRQ restore");
    let unbind = pinned_runner
        .find("vcpu.unbind(&pinned_cpu)")
        .expect("host restoration must use the pinned context");
    assert!(publish < bind && bind < run && run < handle && handle < unbind);
    assert!(
        !pinned_runner.contains("finish_bound_exit"),
        "the typed backend exit must restore through RAII rather than a forgettable finish call"
    );
    assert!(
        !pinned_runner.contains("finish_deferred_run_work"),
        "blocking or deferred VM-exit work must execute after the pinned scope"
    );
    assert!(
        !vcpu.contains("with_current_cpu_set"),
        "CURRENT_VCPU publication must not be callable without a pinned scope"
    );
}

#[test]
fn device_and_hypercall_exit_work_runs_only_after_vcpu_unbind() {
    let architecture_ops = include_str!("../src/architecture/ops.rs");
    let common_exit = include_str!("../src/architecture/exit.rs");
    let sysreg_exit = include_str!("../src/architecture/sysreg.rs");
    let x86_exit = include_str!("../src/arch/x86_64/exit.rs");

    let run_vcpu = architecture_ops
        .split_once("fn run_vcpu(")
        .expect("AxVM must expose the architecture-neutral run boundary")
        .1
        .split_once("fn run_vcpu_pinned")
        .expect("the unpinned and pinned runners must remain separate")
        .0;
    let pinned_runner = architecture_ops
        .split_once("fn run_vcpu_pinned")
        .expect("AxVM must isolate the pinned backend runner")
        .1
        .split_once("pub(crate) fn target_phys_cpu_ids")
        .expect("the pinned runner must remain focused")
        .0;

    assert!(run_vcpu.contains("finish_deferred_run_work"));
    assert!(
        architecture_ops
            .contains("type DeferredRunWork: Copy + 'static + From<CommonDeferredRunWork>;"),
        "deferred work must be plain owned data with no borrowed backend state or destructor"
    );
    assert!(pinned_runner.contains("vcpu.unbind(&pinned_cpu)"));
    assert!(
        !pinned_runner.contains("finish_deferred_run_work"),
        "deferred callbacks must run only after CURRENT_VCPU and the CPU pin are released"
    );

    let common_bound = common_exit
        .split_once("pub(crate) fn handle_mmio_read")
        .expect("common bound exit handlers must exist")
        .1
        .split_once("pub(crate) fn finish_deferred")
        .expect("common exit work must have an explicit unpinned finish boundary")
        .0;
    let sysreg_bound = sysreg_exit
        .split_once("pub(crate) fn handle_read")
        .expect("system-register bound exit handlers must exist")
        .1
        .split_once("pub(crate) fn finish")
        .expect("system-register work must have an unpinned finish boundary")
        .0;
    let port_bound = x86_exit
        .split_once("pub(crate) fn handle_io_read")
        .expect("x86 port bound exit handlers must exist")
        .1
        .split_once("pub(crate) fn finish")
        .expect("x86 port work must have an unpinned finish boundary")
        .0;
    for bound_handlers in [common_bound, sysreg_bound, port_bound] {
        for operation in [
            ".execute()",
            ".get_devices()",
            ".handle_mmio_read(",
            ".handle_mmio_write(",
            ".handle_port_read(",
            ".handle_port_write(",
            ".handle_sys_reg_read(",
            ".handle_sys_reg_write(",
        ] {
            assert!(
                !bound_handlers.contains(operation),
                "bound exit handlers must not execute device or hypercall work: {operation}"
            );
        }
    }

    let common_deferred = common_exit
        .split_once("pub(crate) fn finish_deferred")
        .expect("common exit work must have an explicit unpinned finish boundary")
        .1;
    let sysreg_deferred = sysreg_exit
        .split_once("pub(crate) fn finish")
        .expect("system-register work must have an unpinned finish boundary")
        .1;
    let port_deferred = x86_exit
        .split_once("pub(crate) fn finish")
        .expect("x86 port work must have an unpinned finish boundary")
        .1;
    for (deferred, required) in [
        (common_deferred, ".execute()"),
        (common_deferred, ".handle_mmio_read("),
        (common_deferred, ".handle_mmio_write("),
        (port_deferred, ".handle_port_read("),
        (port_deferred, ".handle_port_write("),
        (sysreg_deferred, ".handle_sys_reg_read("),
        (sysreg_deferred, ".handle_sys_reg_write("),
    ] {
        assert!(
            deferred.contains(required),
            "the unpinned finish path must own deferred operation {required}"
        );
    }
}

#[test]
fn x86_deferred_interrupts_cross_the_runtime_inbox_before_backend_injection() {
    let x86_irq = include_str!("../src/arch/x86_64/irq.rs");
    let deferred_publishers = x86_irq
        .split_once("pub fn queue_due_pit_irq0")
        .expect("x86 PIT publication must exist")
        .1
        .split_once("pub fn drain_bound_pending_ioapic_irqs")
        .expect("deferred publishers must end before the bound IOAPIC drain")
        .0;

    assert!(
        deferred_publishers.contains("publish_pending_interrupt("),
        "deferred x86 device work must publish through the runtime vCPU inbox"
    );
    assert!(
        deferred_publishers.contains("PendingInterrupt::Triggered"),
        "the inbox entry must retain x86 edge/level trigger metadata"
    );
    assert!(
        !deferred_publishers.contains("vcpu.inject_interrupt_with_trigger(")
            && !deferred_publishers.contains(".expect("),
        "work running after unbind must not access or assume an owner-bound backend"
    );

    let architecture_ops = include_str!("../src/architecture/ops.rs");
    let pinned_runner = architecture_ops
        .split_once("fn run_vcpu_pinned")
        .expect("AxVM must isolate its pinned backend runner")
        .1
        .split_once("pub(crate) fn target_phys_cpu_ids")
        .expect("the pinned backend runner must remain focused")
        .0;
    let drain = pinned_runner
        .find("inject_pending_interrupts")
        .expect("the bound owner must drain the runtime interrupt inbox");
    let enter = pinned_runner
        .find("vcpu.run(&pinned_cpu)")
        .expect("the pinned runner must enter the guest");
    assert!(
        drain < enter,
        "queued interrupts must be injected before guest entry"
    );
}

#[test]
fn pending_interrupt_publication_is_vm_instance_bound_and_kick_is_best_effort() {
    let runtime_vcpus = include_str!("../src/runtime/vcpus.rs");
    let publisher = runtime_vcpus
        .split_once("pub(crate) fn publish_pending_interrupt(")
        .expect("AxVM must expose one durable pending-interrupt publisher")
        .1
        .split_once("pub(crate) fn inject_pending_interrupts")
        .expect("publication must remain separate from bound-owner consumption")
        .0;

    assert!(
        publisher.contains("vm: &VMRef") && publisher.contains("vm.with_interrupt_runtime("),
        "deferred publishers must target the existing VM instance and atomically verify its \
         accepting runtime"
    );
    assert!(
        !publisher.contains("get_vm_by_id("),
        "a late callback must not resolve an old VM id to a replacement VM instance"
    );
    assert!(
        publisher.contains("if let Err(error) = crate::host::task::send_ipi(cpu_id)")
            && publisher.contains("interrupt remains published"),
        "a scheduler kick failure must not turn an already durable publication into an error"
    );
}

#[test]
fn bound_pending_interrupt_injection_propagates_failure_to_common_unbind() {
    let architecture_ops = include_str!("../src/architecture/ops.rs");
    let inject = architecture_ops
        .split_once("fn inject_pending_interrupt(")
        .expect("architecture pending-interrupt injection must exist")
        .1
        .split_once("fn on_last_vcpu_exit")
        .expect("pending-interrupt injection must remain focused")
        .0;
    assert!(
        inject.contains("-> AxVmResult") && !inject.contains("Failed to inject queued"),
        "backend injection errors must be returned instead of logged and discarded"
    );

    let pinned_runner = architecture_ops
        .split_once("fn run_vcpu_pinned")
        .expect("AxVM must isolate its pinned backend runner")
        .1
        .split_once("pub(crate) fn target_phys_cpu_ids")
        .expect("the pinned backend runner must remain focused")
        .0;
    assert!(
        pinned_runner.contains("if let Err(error) =")
            && pinned_runner.contains("inject_pending_interrupts")
            && pinned_runner.contains("break Err(error)"),
        "pending-injection errors must join the same mandatory backend-unbind path as run errors"
    );
}

#[test]
fn bound_run_errors_cannot_bypass_backend_unbind() {
    let architecture_ops = include_str!("../src/architecture/ops.rs");
    let pinned_runner = architecture_ops
        .split_once("fn run_vcpu_pinned")
        .expect("AxVM must isolate its pinned backend runner")
        .1
        .split_once("pub(crate) fn target_phys_cpu_ids")
        .expect("the pinned runner must remain focused")
        .0;
    let bound_loop = pinned_runner
        .split_once("let run_result = {")
        .expect("the bound run loop must capture its result")
        .1
        .split_once("// Backend unbind")
        .expect("the bound run loop must end before backend cleanup")
        .0;

    assert!(
        bound_loop.contains("if let Err(error) = bound_vcpu.drain_published_interrupts()")
            && bound_loop.contains("break Err(error)"),
        "an interrupt-publication failure after bind must join the common unbind path"
    );
    assert!(
        !bound_loop.contains("bound_vcpu.drain_published_interrupts()?"),
        "`?` inside the bound loop would return before backend unbind restores host state"
    );
}

#[test]
fn aarch64_deferred_ipi_queues_self_after_current_vcpu_cleanup() {
    let ipi = include_str!("../src/arch/aarch64/ipi.rs");
    let finish = ipi
        .split_once("pub(crate) fn finish")
        .expect("AArch64 must finish guest IPI work outside the pinned scope")
        .1
        .split_once("fn ipi_targets")
        .expect("AArch64 IPI finishing must remain focused")
        .0;

    assert!(
        finish.contains("vm.inject_interrupt_to_vcpu(targets, exit.vector as _)"),
        "all deferred IPI targets, including self, must enter the VM pending queue"
    );
    assert!(
        !finish.contains("inject_current_vcpu_interrupt")
            && !finish.contains("remote_targets.set(vcpu_id, false)"),
        "CURRENT_VCPU is already unpublished when deferred IPI work runs"
    );
}

#[test]
fn deferred_device_identity_uses_live_header_then_task_fallback_outside_irq() {
    let arch = include_str!("../src/arch/mod.rs");
    let vcpu = include_str!("../src/vcpu.rs");
    let host_task = include_str!("../src/host/task.rs");
    let host_arceos = include_str!("../src/host/arceos.rs");
    let aarch64 = include_str!("../src/arch/aarch64/mod.rs");
    let x86_64 = include_str!("../src/arch/x86_64/mod.rs");

    let identity = arch
        .split_once("pub(crate) fn current_vcpu_identity_for_task")
        .expect("deferred device callbacks need a task-context identity API")
        .1
        .split_once("fn select_vcpu_execution_identity")
        .expect("task identity selection must be independently testable")
        .0;
    assert!(identity.contains("vcpu::current_vcpu_identity()"));
    assert!(identity.contains("host::task::in_hard_irq()"));
    assert!(identity.contains("host::task::try_current_task()"));
    assert!(arch.contains("use crate::task::AsVCpuTask;"));
    let imports = vcpu
        .split_once("#[cfg(test)]\ntype Mutex")
        .expect("vCPU module imports must remain separate from test lock aliases")
        .0;
    assert!(
        !imports.contains("task::AsVCpuTask"),
        "the task-extension capability must not compile on unrelated architectures"
    );

    let selector = arch
        .split_once("fn select_vcpu_execution_identity")
        .expect("task identity selection helper must exist")
        .1
        .split_once("pub(crate) fn init_guest_boot_resources")
        .expect("identity selection must remain separate from IRQ publication")
        .0;
    assert_in_order(
        selector,
        &[
            "if live_identity.is_some()",
            "return Ok(live_identity)",
            "if in_hard_irq",
            "return Ok(None)",
            "task_identity()",
        ],
    );
    assert!(host_task.contains("pub(crate) fn in_hard_irq() -> bool"));
    assert!(
        host_task
            .contains("pub(crate) fn try_current_task() -> Result<Option<CurrentTask>, TaskError>")
    );
    assert!(host_task.contains("!in_hard_irq()"));
    assert!(host_arceos.contains("modules::ax_hal::irq::in_irq_context()"));
    assert!(host_arceos.contains("modules::ax_task::current_thread_handle()"));
    assert!(!host_arceos.contains("current_thread_handle()\n        .ok()"));
    assert!(arch.contains("pub(crate) struct VcpuExecutionIdentity"));
    assert!(!vcpu.contains("pub(crate) struct VcpuExecutionIdentity"));
    assert!(!vcpu.contains("pub(crate) fn current_vcpu_identity_for_task"));

    for architecture in [aarch64, x86_64] {
        assert!(
            architecture.contains("current_vcpu_identity_for_task()"),
            "device host callbacks must use the normal-task identity boundary after unbind"
        );
    }
}

#[test]
fn riscv_bound_exit_captures_the_physical_irq_before_raii_restore() {
    let riscv = include_str!("../src/arch/riscv64/mod.rs");
    let handler = riscv
        .split_once("fn handle_vcpu_exit_bound")
        .expect("RISC-V bound exit handler must exist")
        .1
        .split_once("fn finish_deferred_run_work")
        .expect("RISC-V bound exit handler must remain focused")
        .0;
    let view = handler
        .find("let exit_event = exit.event()")
        .expect("RISC-V must read only the Copy event view");
    let capture = handler
        .find("capture_bound_external_interrupt")
        .expect("physical IRQ capture must remain in the bound handler");
    let restore = handler
        .find("drop(exit)")
        .expect("RISC-V must explicitly finish the RAII exit after capture");
    assert!(view < capture && capture < restore);
}

#[test]
fn riscv_completion_unmask_and_guest_entry_share_one_outer_irq_guard() {
    let riscv = include_str!("../src/arch/riscv64/mod.rs");
    let run = riscv
        .split_once("fn run<'cpu>")
        .expect("RISC-V backend run adapter must exist")
        .1
        .split_once("fn bind")
        .expect("RISC-V run adapter must remain focused")
        .0;
    let take_completions = run
        .find("take_completed_claim_batch")
        .expect("the fixed owner must take completions in task context");
    let disable = run
        .find("let irq_guard = IrqGuard::new()")
        .expect("adapter must mask IRQs before physical unmask");
    let unmask = run
        .find("self.unmask_completed_physical_irqs")
        .expect("adapter must unmask completed sources while IRQs stay disabled");
    let sync = run
        .find("self.sync_vplic_line()")
        .expect("adapter must synchronize the owner vPLIC line");
    let enter = run
        .find("self.backend.run(cpu_pin)")
        .expect("adapter must enter the guest through the typed backend");
    assert!(take_completions < disable && disable < unmask && unmask < sync && sync < enter);

    let bound_exit = riscv
        .split_once("impl Drop for AxvmRiscvBoundExit")
        .expect("AxVM's RISC-V bound exit must own explicit drop ordering")
        .1
        .split_once("impl AxvmRiscvVcpu")
        .expect("bound-exit drop must remain separate from vCPU behavior")
        .0;
    assert_in_order(
        bound_exit,
        &["drop(self.backend.take())", "drop(self.irq_guard.take())"],
    );
}

#[test]
fn riscv_passthrough_irq_affinity_has_one_deterministic_vm_owner() {
    let riscv = include_str!("../src/arch/riscv64/mod.rs");
    let first_run = riscv
        .split_once("fn before_first_run")
        .expect("RISC-V first-run hook must exist")
        .1
        .split_once("fn vcpu_affinities")
        .expect("RISC-V first-run hook must remain focused")
        .0;
    let owner_check = first_run
        .find("vcpu.id() != 0")
        .expect("only vCPU0 may install VM-wide physical IRQ affinity");
    let pin = first_run
        .find("let route_guard = PreemptGuard::new()")
        .expect("one CPU pin must cover the complete passthrough route transaction");
    let live_cpu = first_run
        .find("current_cpu_index(route_guard.cpu_pin())")
        .expect("the route owner must verify its live pinned host CPU");
    let configure = first_run
        .find("route.install_platform_route(cpu_id, &irq_sources, route_guard.cpu_pin())")
        .expect("RISC-V VM must install the transactional passthrough route");
    assert!(owner_check < pin && pin < live_cpu && live_cpu < configure);

    let route = include_str!("../src/arch/riscv64/irq.rs");
    let route_transaction = include_str!("../src/arch/riscv64/route_transaction.rs");
    assert!(
        route
            .contains("static PLATFORM_VPLIC_ROUTE_CONTROL: RouteControl<PlatformVplicRouteState>")
            && route_transaction
                .contains("pub(super) type RouteControl<T> = ax_kspin::SpinNoPreempt<T>"),
        "AxVM route ownership needs an explicit transactional state machine"
    );

    let platform = include_str!("../../../platforms/axplat-dyn/src/irq.rs");
    assert!(
        platform.contains("static VIRTUAL_IRQ_ROUTE_CONTROL: SpinNoPreempt<VirtualIrqRouteState>"),
        "platform route ownership needs an explicit transactional state machine"
    );
}

#[test]
fn riscv_vm_target_install_leases_unclaimed_passthrough_sources() {
    let platform_irq = include_str!("../../../platforms/axplat-dyn/src/irq.rs");
    let prepare = platform_irq
        .split_once("pub fn prepare_virtual_irq_targets")
        .expect("RISC-V target preparation must remain explicit")
        .1
        .split_once("pub fn activate_virtual_irq_targets")
        .expect("RISC-V preparation must remain separate from activation")
        .0;
    assert!(
        !prepare.contains("VIRTUAL_IRQ_ROUTE_CONTROL.lock()"),
        "endpoint allocation and controller leasing must run outside the route-state lock"
    );
    assert!(
        prepare.contains("lease_riscv_plic_irq_endpoints(&new_irqs, affinity)"),
        "the control plane must atomically lease the complete source-owned endpoint batch"
    );
    assert!(
        prepare.contains("VIRTUAL_IRQ_ENDPOINTS[source].call_once"),
        "every validated endpoint must be published once while still masked"
    );
    for forbidden in ["irq_set_affinity", "irq_set_enable", ".endpoint.unmask()"] {
        assert!(
            !prepare.contains(forbidden),
            "preparation must not activate or generically reconfigure an endpoint: {forbidden}"
        );
    }

    assert_in_order(
        prepare,
        &[
            "lease_riscv_plic_irq_endpoints(&new_irqs, affinity)",
            "preparation.begin_irreversible()",
            "VIRTUAL_IRQ_ENDPOINTS[source].call_once",
            "preparation.publish()",
        ],
    );

    let activate = platform_irq
        .split_once("pub fn activate_virtual_irq_targets")
        .expect("RISC-V target activation must remain explicit")
        .1
        .split_once("struct IrqIfImpl")
        .expect("RISC-V activation must remain focused")
        .0;
    assert!(
        !activate.contains("VIRTUAL_IRQ_ROUTE_CONTROL.lock()"),
        "endpoint MMIO activation must run outside the route-state lock"
    );
    assert_in_order(
        activate,
        &[
            "for &source in irq_sources",
            r#".expect("a published RISC-V route must own every endpoint before activation")"#,
            "for &source in irq_sources",
            "activate_virtual_irq_endpoint(source)",
        ],
    );

    let axvm_route = include_str!("../src/arch/riscv64/irq.rs");
    let transaction = axvm_route
        .split_once("pub(crate) fn install_platform_route")
        .expect("AxVM must own one monitor-wide route transaction")
        .1
        .split_once("fn same_binding")
        .expect("route transaction must remain focused")
        .0;
    assert!(
        !transaction.contains("PLATFORM_VPLIC_ROUTE_CONTROL.lock()"),
        "platform preparation and activation must run outside the AxVM route-state lock"
    );
    assert_in_order(
        transaction,
        &[
            "prepare_route_if_available",
            "preparation_permit.begin_irreversible()",
            "install_platform_owner",
            "PLATFORM_VPLIC_ROUTE.call_once",
            "activate_virtual_irq_targets",
        ],
    );
    assert!(transaction.contains("PlatformVplicRoute::new"));
    assert!(axvm_route.contains("target_cpu: usize"));
    assert!(axvm_route.contains("irq_sources: Box<[u32]>"));
}

#[test]
fn riscv_plic_source_lease_prepares_masked_then_activates_after_publication() {
    let plic = include_str!("../../../platforms/somehal/src/arch/riscv64/plic.rs");
    let endpoint = plic
        .split_once("pub struct RiscvPlicIrqEndpoint")
        .expect("the physical PLIC endpoint capability must exist")
        .0
        .rsplit_once("/// Immutable IRQ-side capability")
        .expect("the endpoint capability must retain its ownership contract")
        .1;
    assert!(!endpoint.contains("derive(Clone"));
    assert!(!endpoint.contains("derive(Copy"));
    assert!(plic.contains("pub fn mask(&self)"));
    assert!(plic.contains("pub fn unmask(&self)"));

    let lease = plic
        .rsplit_once("fn lease_irq_endpoints(")
        .expect("the physical PLIC batch lease must be explicit")
        .1
        .split_once("fn contexts_for_source")
        .expect("the source batch lease must remain focused")
        .0;
    let precommit = lease
        .split_once("// PLIC priority zero")
        .expect("the PLIC batch must mark its commit boundary")
        .0;
    assert!(
        !precommit.contains("set_priority"),
        "lease validation must not transiently unmask a live host source"
    );
    assert_in_order(
        lease,
        &[
            "for &hwirq in hwirqs",
            "prepared.push(source)",
            "for &source in &prepared",
            "handler.set_priority(source, 0)",
            "plic_fence_output_to_output()",
            "for source in prepared",
            "self.disable_source_contexts(source)",
            "self.affinity_by_source[source.get() as usize] = affinity",
            "self.inner.enable(source, context)",
            "self.leased_by_source[source.get() as usize] = true",
            "restore_priority: DEFAULT_PRIORITY",
        ],
    );
    assert!(plic.contains("return Err(rdif_intc::IrqError::Busy)"));

    let platform_irq = include_str!("../../../platforms/axplat-dyn/src/irq.rs");
    let platform_endpoint = platform_irq
        .split_once("struct RiscvVirtualIrqEndpoint")
        .expect("the platform must retain one source-owned endpoint")
        .0
        .rsplit_once("#[cfg")
        .expect("the platform endpoint must remain target-gated")
        .1;
    assert!(!platform_endpoint.contains("derive(Clone"));
    assert!(!platform_endpoint.contains("derive(Copy"));
    let unmask = platform_irq
        .split_once("pub fn unmask_virtual_irq")
        .expect("guest completion unmask path must exist")
        .1
        .split_once("fn validate_pinned_virtual_irq_target")
        .expect("guest completion unmask path must remain focused")
        .0;
    assert_in_order(
        unmask,
        &[
            "ForwardedGeneration::new(claim.generation)",
            "FORWARDED_IRQ_STATE[source].begin_unmask(generation)",
            "endpoint.endpoint.unmask()",
            "FORWARDED_IRQ_STATE[source].finish_unmask(permit)",
        ],
    );
}

#[test]
fn riscv_unbound_passthrough_irq_uses_one_preinstalled_wake_route() {
    let platform_irq = include_str!("../../../platforms/axplat-dyn/src/irq.rs");
    let riscv = include_str!("../src/arch/riscv64/mod.rs");
    let vplic_irq = include_str!("../src/arch/riscv64/irq.rs");

    let handle = platform_irq
        .split_once("fn handle(vector: TrapVector)")
        .expect("platform IRQ entry must exist")
        .1
        .split_once("fn send_ipi")
        .expect("platform IRQ entry must remain focused")
        .0;
    let forward = handle
        .find("forward_claimed_virtual_irq")
        .expect("an unbound guest-owned source must be forwarded from host hard IRQ");
    let host_dispatch = handle
        .find("dispatch_claimed_host_irq")
        .expect("non-guest sources must retain normal host dispatch");
    assert!(forward < host_dispatch);

    assert!(platform_irq.contains("register_virtual_irq_sink"));
    assert!(riscv.contains("fn register_platform_irq_injector"));
    assert!(riscv.contains("RiscvPlatformIrq::register_sink(forward_unbound_physical_irq)"));
    assert!(vplic_irq.contains("PLATFORM_VPLIC_ROUTE"));
    assert!(vplic_irq.contains("install_platform_route"));
    assert!(vplic_irq.contains("forward_unbound_physical_irq"));

    let route = vplic_irq
        .split_once("fn forward_unbound_physical_irq")
        .expect("the hard-IRQ route must be explicit")
        .1
        .split_once("fn encode_claim")
        .expect("the hard-IRQ route must remain focused")
        .0;
    for forbidden in ["current_vm_id", "get_vm_by_id", "with_runtime", "Box::new"] {
        assert!(
            !route.contains(forbidden),
            "hard-IRQ passthrough routing must not recover ownership through {forbidden}"
        );
    }
}

#[test]
fn riscv_passthrough_hard_irq_uses_endpoint_and_lock_free_ingress_only() {
    let platform_irq = include_str!("../../../platforms/axplat-dyn/src/irq.rs");
    let plic = include_str!("../../../platforms/somehal/src/arch/riscv64/plic.rs");
    let riscv = include_str!("../src/arch/riscv64/mod.rs");
    let vplic_irq = include_str!("../src/arch/riscv64/irq.rs");
    let ingress = include_str!("../src/arch/riscv64/forwarded_ingress.rs");

    let handle = platform_irq
        .split_once("fn handle(vector: TrapVector)")
        .expect("platform IRQ entry must exist")
        .1
        .split_once("fn send_ipi")
        .expect("platform IRQ entry must remain focused")
        .0;
    assert_in_order(
        handle,
        &[
            "active.controller_id()",
            "forward_claimed_virtual_irq",
            "active.id()",
        ],
    );

    let mask = platform_irq
        .split_once("fn mask_forwarded_virtual_irq")
        .expect("forwarded PLIC mask path must exist")
        .1
        .split_once("fn forward_claimed_virtual_irq")
        .expect("forwarded PLIC mask path must remain focused")
        .0;
    assert_in_order(
        mask,
        &[
            "RiscvPlicSource::from_irq(controller_irq)",
            "VIRTUAL_IRQ_ENDPOINTS[source].get()",
        ],
    );
    assert!(mask.contains("VIRTUAL_IRQ_ENDPOINTS[source].get()"));
    assert!(mask.contains("endpoint.mask()"));
    for forbidden in [
        "irq_set_enable",
        "domain_by_kind",
        "rdrive",
        ".lock()",
        "warn!",
        "debug!",
        "format!",
    ] {
        assert!(
            !mask.contains(forbidden),
            "hard-IRQ physical masking must not call {forbidden}"
        );
    }

    let forward = vplic_irq
        .split_once("pub(crate) fn forward_physical_irq")
        .expect("physical forwarding path must exist")
        .1
        .split_once("pub(crate) fn take_completed_claim_batch")
        .expect("physical forwarding path must remain focused")
        .0;
    assert!(forward.contains("self.notifications.ingress.publish"));
    assert!(forward.contains("self.notifications.publish_owner"));
    for forbidden in [
        ".lock()",
        "set_forwarded_pending",
        "refresh_all_guest_context_lines",
        "publish_changed_contexts",
        "warn!",
        "format!",
        "Box::new",
        "Vec::",
    ] {
        assert!(
            !forward.contains(forbidden),
            "hard-IRQ vPLIC ingress must not call {forbidden}"
        );
    }

    assert!(ingress.contains("pub(crate) const FORWARDED_IRQ_DRAIN_BATCH: usize = 64"));
    assert!(ingress.contains("compare_exchange(0, claim"));
    assert!(ingress.contains("fetch_or(bit, Ordering::Release)"));
    assert!(ingress.contains("swap(0, Ordering::AcqRel)"));
    assert!(ingress.contains("rearm_after_drain"));

    let publish = ingress
        .split_once("pub(crate) fn publish")
        .expect("lock-free ingress publication must exist")
        .1
        .split_once("pub(crate) fn take_batch")
        .expect("ingress publication must remain focused")
        .0;
    assert_in_order(
        publish,
        &[
            "compare_exchange(0, claim",
            "fetch_or(bit, Ordering::Release)",
            "notification_armed.swap(true, Ordering::AcqRel)",
        ],
    );
    for forbidden in [".lock()", "warn!", "debug!", "format!", "Box::", "Vec::"] {
        assert!(
            !publish.contains(forbidden),
            "hard-IRQ ingress publication must not call {forbidden}"
        );
    }

    let owner_drain = vplic_irq
        .split_once("pub(crate) fn drain_forwarded_ingress")
        .expect("owner-thread ingress drain must exist")
        .1
        .split_once("pub(crate) unsafe extern \"C\" fn forward_unbound_physical_irq")
        .expect("owner-thread ingress drain must remain focused")
        .0;
    assert_eq!(
        owner_drain.matches("set_forwarded_pending_batch").count(),
        1
    );
    assert!(owner_drain.contains("if !self.is_platform_owner()"));
    assert!(!owner_drain.contains("set_forwarded_pending("));
    assert_in_order(
        owner_drain,
        &[
            "take_batch()",
            "set_forwarded_pending_batch",
            "publish_changed_contexts",
            "rearm_after_drain",
        ],
    );

    let run = riscv
        .split_once("fn run<'cpu>")
        .expect("RISC-V vCPU run path must exist")
        .1
        .split_once("fn bind")
        .expect("RISC-V vCPU run path must remain focused")
        .0;
    assert_in_order(
        run,
        &[
            "drain_forwarded_ingress",
            "take_completed_claim_batch",
            "let irq_guard = IrqGuard::new()",
            "unmask_completed_physical_irqs",
            "self.sync_vplic_line()",
        ],
    );

    for fence in ["fence rw, ow", "fence ow, ow", "fence ow, rw"] {
        assert!(
            plic.contains(fence),
            "PLIC endpoint publication and MMIO must retain {fence} ordering"
        );
    }
}

#[test]
fn axvm_percpu_services_require_live_cpu_pins() {
    let vcpu = include_str!("../src/vcpu.rs");
    let percpu = include_str!("../src/percpu.rs");
    let timer = include_str!("../src/timer.rs");
    let host = include_str!("../src/host/arceos.rs");

    for (name, source) in [("percpu", percpu), ("timer", timer)] {
        for unchecked_access in [
            "current_ptr_unchecked()",
            "current_ref_raw()",
            "current_ref_mut_raw()",
        ] {
            assert!(
                !source.contains(unchecked_access),
                "AxVM {name} service must use a live CpuPin instead of {unchecked_access}",
            );
        }
    }

    assert!(vcpu.contains("pub(crate) const fn cpu_index(&self) -> CpuIndex"));
    assert!(vcpu.contains("pub(crate) const fn cpu_index_usize(&self) -> usize"));

    let prepared_state = timer
        .split_once("struct PreparedVmTimerState")
        .expect("the timer preparation object must exist")
        .1
        .split_once('}')
        .expect("the timer preparation object must have a body")
        .0;
    assert!(
        !prepared_state.contains("owner_cpu"),
        "allocation-only timer preparation must not capture a migratable CPU identity",
    );
    let prepare_signature = timer
        .split_once("fn prepare_percpu")
        .expect("the timer preparation function must exist")
        .1
        .split_once('{')
        .expect("the timer preparation function must have a body")
        .0;
    assert!(
        !prepare_signature.contains("owner_cpu"),
        "timer preparation must allocate before a CPU is pinned",
    );
    assert!(
        !percpu.contains("this_cpu_id"),
        "per-CPU initialization must derive its owner from PinnedCpuContext",
    );
    assert!(vcpu.contains("self.identity.cpu_index"));
    for operation in ["fn init_current_cpu", "fn enable_current_cpu"] {
        let signature = percpu
            .split_once(operation)
            .unwrap_or_else(|| panic!("missing AxVM per-CPU operation: {operation}"))
            .1
            .split_once('{')
            .expect("the per-CPU operation must have a body")
            .0;
        assert!(
            signature.contains("PinnedCpuContext<'_>"),
            "{operation} must use one pinned CPU identity for addressing and ownership",
        );
    }

    let initialize = host
        .split_once("fn enable_current_cpu_services")
        .expect("host initialization must return the identity it actually enabled")
        .1
        .split_once("impl HostPlatform")
        .expect("the private initialization helper must precede the host trait implementation")
        .0;
    let prepare = initialize
        .find("timer::prepare_percpu()")
        .expect("timer storage must be allocated before pinning the CPU");
    let pin = initialize
        .find("let preempt_guard = PreemptGuard::new();")
        .expect("backend initialization must pin the CPU");
    let context = initialize
        .find("PinnedCpuContext::new(preempt_guard.cpu_pin())")
        .expect("backend initialization must construct one pinned identity");
    let owner = initialize
        .find("pinned_cpu.cpu_index_usize()")
        .expect("the enabled CPU identity must come from the CPU-area header");
    let init = initialize
        .find("init_current_cpu(&pinned_cpu)")
        .expect("the backend must be constructed while pinned");
    let enable = initialize
        .find("enable_current_cpu(&pinned_cpu)")
        .expect("the backend must be enabled while pinned");
    let install = initialize
        .find("timer::install_percpu(")
        .expect("timer storage must be published only after backend enablement");
    let unpin = initialize
        .find("drop(preempt_guard)")
        .expect("task creation must happen after unpinning");
    let start = initialize
        .find("timer::start_percpu_worker")
        .expect("each enabled CPU must receive one pinned timer worker");
    let publish = initialize
        .find("mark_cpu_enabled")
        .expect("a CPU must be published only after all services are live");

    assert!(prepare < pin);
    assert!(pin < context && context < owner && owner < init);
    assert!(init < enable && enable < install);
    assert!(install < unpin && unpin < start && start < publish);
    assert!(
        initialize.contains("let owner_cpu = enable_result?;"),
        "failed prepared state must be released only after leaving the pinned scope",
    );
}

#[test]
fn riscv_capability_probe_and_vm_layout_are_cpu_owned() {
    let detect = include_str!("../../riscv_vcpu/src/detect.rs");
    let riscv_vm = include_str!("../src/arch/riscv64/vm.rs");

    assert!(
        detect.contains("pub fn max_guest_page_table_levels(cpu_pin: &CpuPin)"),
        "RISC-V hgatp probing must require an explicit CPU pin"
    );
    let probe = detect
        .split_once("fn with_detect_trap")
        .expect("RISC-V extension probe must exist")
        .1
        .split_once("fn read_cpu_anchor")
        .expect("probe setup must end before the anchor helper")
        .0;
    assert!(
        probe.find("init_detect_trap").is_some_and(|disable| {
            probe
                .find("read_cpu_anchor")
                .is_some_and(|read| disable < read)
        }),
        "the probe must disable local IRQs before reading sscratch"
    );
    assert!(
        !riscv_vm.contains("riscv_vcpu::max_guest_page_table_levels()"),
        "VM construction must consume published per-CPU capabilities instead of probing live CSRs"
    );
    assert!(
        !riscv_vm.contains("unwrap_or_else(riscv_vcpu::max_guest_page_table_levels)"),
        "missing target-CPU capability data must be an error, not a current-CPU fallback"
    );
}

#[test]
fn live_vcpu_interrupt_injection_requires_the_bound_cpu_owner() {
    let vcpu = include_str!("../src/vcpu.rs");
    let axvm_riscv = include_str!("../src/arch/riscv64/mod.rs");
    let riscv = include_str!("../../riscv_vcpu/src/vcpu.rs");
    let loongarch = include_str!("../../loongarch_vcpu/src/vcpu.rs");

    assert!(
        vcpu.contains("pub(crate) struct BoundVcpu")
            && vcpu.contains("impl<'scope, 'cpu, A: VmArchVcpuOps> BoundVcpu")
            && vcpu.contains("BoundOwnerOnly"),
        "AxVM must require an unforgeable bound-owner capability for live-backend injection"
    );
    let bound_capability = vcpu
        .split_once("impl<'scope, 'cpu, A: VmArchVcpuOps> BoundVcpu")
        .expect("bound vCPU capability implementation must exist")
        .1
        .split_once("impl<A: VmArchVcpuOps> AxVCpu")
        .expect("bound-only operations must remain separate from the general vCPU API")
        .0;
    assert!(
        bound_capability.contains("fn inject_interrupt(")
            && bound_capability.contains("fn inject_interrupt_with_trigger(")
            && bound_capability.contains("fn drain_published_interrupts("),
        "all live interrupt delivery must require the bound-owner capability"
    );
    let riscv_inject = riscv
        .split_once("pub fn inject_interrupt")
        .expect("RISC-V injection method must exist")
        .1
        .split_once("pub fn set_return_value")
        .expect("RISC-V injection method must remain focused")
        .0;
    assert!(
        !riscv_inject.contains("hvip::set_") && !riscv_inject.contains("hvip::read()"),
        "an unbound RISC-V vCPU may only update its saved pending state"
    );
    let loongarch_inject = loongarch
        .split_once("pub fn inject_interrupt")
        .expect("LoongArch injection method must exist")
        .1
        .split_once("pub fn set_return_value")
        .expect("LoongArch injection method must remain focused")
        .0;
    assert!(
        !loongarch_inject.contains("registers::inject_interrupt"),
        "an unbound LoongArch vCPU may only update vCPU-owned pending state"
    );

    let external_irq = axvm_riscv
        .split_once("fn capture_bound_external_interrupt")
        .expect("RISC-V bound external interrupt capture must exist")
        .1
        .split_once("fn handle_riscv_nested_page_fault")
        .expect("external interrupt capture must remain focused")
        .0;
    assert!(external_irq.contains("RiscvPlatformIrq::claim_and_mask(vector)"));
    assert!(external_irq.contains("forward_physical_irq(claim)"));
    assert!(
        !external_irq.contains("vcpu.bind") && !external_irq.contains("vcpu.unbind"),
        "device IRQ capture must publish software state without rebinding live guest CSRs"
    );
    assert!(!external_irq.contains("dispatch_host_irq"));
    assert!(!external_irq.contains("latch_hvip_from_hw"));
    assert!(!riscv.contains("pub fn latch_hvip_from_hw"));
}

#[test]
fn riscv_vplic_routes_software_context_state_to_the_bound_owner() {
    let irq = include_str!("../src/arch/riscv64/irq.rs");
    let riscv = include_str!("../src/arch/riscv64/mod.rs");
    let vm = include_str!("../src/arch/riscv64/vm.rs");
    let vplic = include_str!("../../riscv_vplic/src/devops_impl.rs");

    assert!(irq.contains("take_context_notification"));
    assert!(irq.contains("ThreadWakeHandle"));
    assert!(irq.contains("ForwardedIrqIngress"));
    assert!(irq.contains("FixedOwnerContext"));
    assert!(irq.contains("take_completed_claim_batch"));
    assert!(irq.contains("forward_physical_irq"));
    for forbidden in ["queue_interrupt", "get_vm_by_id", "with_runtime"] {
        assert!(
            !irq.contains(forbidden),
            "the vPLIC IRQ sink runs while VM resources may be locked and must not re-enter them \
             through {forbidden}"
        );
    }
    assert!(vm.contains("interrupt_resources.vplic"));
    assert!(riscv.contains("fn sync_vplic_line"));
    assert!(riscv.contains("hvip::set_vseip"));
    assert!(riscv.contains("hvip::clear_vseip"));
    for owner_only in ["take_completed_claim_batch", "drain_forwarded_ingress"] {
        let body = irq
            .split_once(&format!("pub(crate) fn {owner_only}"))
            .unwrap_or_else(|| panic!("missing owner-only method {owner_only}"))
            .1
            .split_once("\n    }")
            .expect("owner-only method must have a focused body")
            .0;
        assert!(
            body.contains("if !self.is_platform_owner()"),
            "{owner_only} must reject nonowner vCPUs before consuming VM-global state"
        );
    }
    assert!(
        !vplic.contains("riscv_h::register")
            && !vplic.contains("hvip::")
            && !vplic.contains("vscause::"),
        "the vPLIC model must never access the bound CPU's live interrupt CSRs"
    );
}

#[test]
fn riscv_external_irq_claim_stays_inside_the_bound_owner_scope() {
    let riscv = include_str!("../src/arch/riscv64/mod.rs");
    let platform_irq = include_str!("../../../platforms/axplat-dyn/src/irq.rs");
    let bound_exit = riscv
        .split_once("fn handle_vcpu_exit_bound")
        .expect("RISC-V bound exit handler must exist")
        .1
        .split_once("fn finish_deferred_run_work")
        .expect("RISC-V bound exit handler must remain focused")
        .0;
    assert!(bound_exit.contains("RiscvVmExit::ExternalInterrupt"));
    assert!(bound_exit.contains("capture_bound_external_interrupt"));
    assert!(bound_exit.contains("BoundVcpuExit::Continue"));
    assert!(
        !bound_exit.contains("RiscvDeferredRunWork::ExternalInterrupt"),
        "a physical PLIC source must be claimed before unbind restores host IRQ delivery"
    );

    let deferred = riscv
        .split_once("fn finish_deferred_run_work")
        .expect("RISC-V deferred work hook must exist")
        .1
        .split_once("fn handle_riscv_nested_page_fault")
        .expect("RISC-V deferred work hook must remain focused")
        .0;
    assert!(
        !deferred.contains("dispatch_host_irq"),
        "re-claiming a PLIC source after unbind races normal host IRQ delivery"
    );
    assert!(
        platform_irq.contains("claim_and_mask_virtual_irq")
            && platform_irq.contains("endpoint.endpoint.mask()"),
        "a forwarded level PLIC source must be claimed and masked before software delivery"
    );
    assert!(platform_irq.contains("unmask_virtual_irq"));

    let bound_capture = riscv
        .split_once("fn capture_bound_external_interrupt")
        .expect("RISC-V bound physical IRQ capture must exist")
        .1
        .split_once("fn handle_riscv_nested_page_fault")
        .expect("RISC-V bound physical IRQ capture must remain focused")
        .0;
    for forbidden in [
        "current_vm_id",
        "get_vm_by_id",
        "pulse_interrupt",
        "queue_interrupt",
    ] {
        assert!(
            !bound_capture.contains(forbidden),
            "bound IRQ dispatch must not recover its target through {forbidden}"
        );
    }
    assert!(
        bound_capture.contains("return Err(crate::AxVmError::interrupt"),
        "a captured and masked physical source must fail the vCPU when owner publication fails"
    );
}

#[test]
fn axvm_ipi_routes_hold_one_cpu_pin_through_identity_and_send() {
    let host = include_str!("../src/host/arceos.rs");
    let host_traits = include_str!("../src/host/traits.rs");

    assert!(
        !host_traits.contains("fn this_cpu_id"),
        "AxVM host capabilities must not expose a migratable CPU identity snapshot",
    );

    let direct = host
        .split_once("pub(crate) fn send_ipi(cpu_id: usize)")
        .expect("AxVM must provide direct host IPI routing")
        .1
        .split_once("fn send_ipi_to_all_except_current")
        .expect("direct routing must end before broadcast routing")
        .0;
    let direct_pin = direct
        .find("let preempt_guard = PreemptGuard::new();")
        .expect("direct IPI routing must pin the caller");
    let direct_identity = direct
        .find("this_cpu_id_pinned(preempt_guard.cpu_pin())")
        .expect("direct IPI routing must read identity through its live pin");
    let direct_send = direct
        .find("modules::ax_hal::irq::send_ipi")
        .expect("direct IPI routing must send while still pinned");
    assert!(direct_pin < direct_identity && direct_identity < direct_send);
    assert!(!direct.contains("this_cpu_id()"));
    assert!(!direct.contains("drop(preempt_guard)"));

    let broadcast = host
        .split_once("fn send_ipi_to_all_except_current")
        .expect("AxVM must provide broadcast host IPI routing")
        .1
        .split_once("pub fn shutdown_host_filesystems")
        .expect("broadcast routing must end before filesystem shutdown")
        .0;
    let broadcast_pin = broadcast
        .find("let preempt_guard = PreemptGuard::new();")
        .expect("broadcast IPI routing must pin the caller");
    let broadcast_identity = broadcast
        .find("this_cpu_id_pinned(preempt_guard.cpu_pin())")
        .expect("broadcast IPI exclusion must use its live pin");
    let broadcast_send = broadcast
        .find("send_ipi_from_pinned(cpu, &preempt_guard)")
        .expect("broadcast IPI routing must reuse its original pin for every target");
    assert!(broadcast_pin < broadcast_identity && broadcast_identity < broadcast_send);
    assert!(!broadcast.contains("this_cpu_id()"));
    assert!(!broadcast.contains("drop(preempt_guard)"));

    let all_cpus = host
        .split_once("fn enable_virtualization_on_all_cpus")
        .expect("all-CPU virtualization initialization must exist")
        .1;
    assert!(all_cpus.contains("let current_cpu = self.enable_current_cpu_services()?;"));
    assert!(all_cpus.contains("let enabled_cpu = host"));
    assert!(all_cpus.contains("enabled_cpu, cpu_id"));
    assert!(
        !all_cpus.contains("self.this_cpu_id()"),
        "the all-CPU loop must not compare targets with a migratable CPU snapshot",
    );
    assert!(all_cpus.contains("send_ipi(cpu_id)?;"));
    assert!(!all_cpus.contains("if cpu_id != self.this_cpu_id()"));
}

#[test]
fn axvm_test_linker_keeps_cpu_area_header_at_template_offset_zero() {
    let linker_script = include_str!("../percpu-test.x");

    assert!(linker_script.contains("__AX_PERCPU_INIT_START = .;"));
    assert!(linker_script.contains("KEEP(*(.ax_percpu.init))"));
    assert!(linker_script.contains("__AX_PERCPU_INIT_END = .;"));
    assert!(
        linker_script.contains("__AX_CPU_AREA_PREFIX == _percpu_load_start"),
        "the AxVM test image must enforce the production CPU-area header ABI",
    );
}

#[test]
fn timer_wheel_delivery_uses_a_task_context_worker_instead_of_irq_callbacks() {
    let timer = include_str!("../src/timer.rs");
    let public_api = include_str!("../src/lib.rs");
    let host_traits = include_str!("../src/host/traits.rs");
    let architecture_capabilities = include_str!("../src/architecture/capabilities.rs");
    let loongarch_capabilities = include_str!("../src/arch/loongarch64/capabilities.rs");

    assert!(timer.contains("wait_queue_wait_until_deadline"));
    assert!(timer.contains("timer_worker_main"));
    assert!(timer.contains("VmTimerToken"));
    assert!(timer.contains("owner_cpu"));
    assert!(timer.contains("generation"));
    assert!(timer.contains("STATE_DISPATCHING"));
    assert!(timer.contains("remote_ptr"));
    assert!(!timer.contains("TimerList"));
    assert!(!timer.contains("SpinNoIrq"));
    assert!(!timer.contains("set_oneshot_timer"));
    assert!(!public_api.contains("pub fn check_timer_events"));
    assert!(!host_traits.contains("set_oneshot_timer"));
    assert!(!architecture_capabilities.contains("register_timer_callback"));
    assert!(!loongarch_capabilities.contains("register_timer_callback"));
    assert!(!loongarch_capabilities.contains("ax_task::register_timer_callback"));
}

#[test]
fn architecture_timer_callbacks_keep_vcpu_identity_and_never_reprogram_host_irq_state() {
    let arm_timer = include_str!("../../arm_vgic/src/vtimer/cntp_tval_el0.rs");
    let loongarch_idle = include_str!("../src/arch/loongarch64/idle.rs");

    assert!(arm_timer.contains("current_vm_id"));
    assert!(arm_timer.contains("current_vcpu_id"));
    assert!(arm_timer.contains("queue_virtual_interrupt"));
    assert!(!arm_timer.contains("hardware_inject_virtual_interrupt"));

    for forbidden in [
        "set_timer_irq_enabled",
        "enable_irqs",
        "disable_irqs",
        "busy_wait",
        "check_timer_events",
    ] {
        assert!(
            !loongarch_idle.contains(forbidden),
            "LoongArch vCPU idle must not manipulate host timer/IRQ state through {forbidden}",
        );
    }
    assert!(loongarch_idle.contains("thread::sleep"));
}

#[test]
fn current_vcpu_publication_never_reconstructs_a_generic_backend_reference() {
    let vcpu = include_str!("../src/vcpu.rs");
    let manager = include_str!("../src/manager.rs");

    assert!(
        vcpu.contains("AtomicPtr<CurrentVcpuHeader>"),
        "the per-CPU slot must publish a backend-independent immutable header"
    );
    assert!(
        !vcpu.contains("AtomicPtr<u8>")
            && !vcpu.contains("pointer as *const AxVCpu")
            && !vcpu.contains("with_current_vcpu<A:"),
        "a type-erased current-vCPU pointer must never be cast back to a caller-selected backend"
    );
    assert!(
        manager.contains("current_vcpu_identity")
            && manager.contains("publish_current_vcpu_interrupt")
            && !manager.contains("with_current_vcpu::<ArchVCpu")
            && !manager.contains("vcpu.inject_interrupt(vector)"),
        "identity queries and hard-IRQ injection must use the stable header, not &mut backend"
    );
}

#[test]
fn current_vcpu_irq_path_only_publishes_preallocated_pending_state() {
    let vcpu = include_str!("../src/vcpu.rs");
    let header = include_str!("../src/current_vcpu.rs");
    let manager = include_str!("../src/manager.rs");

    assert!(vcpu.contains("AtomicPtr<CurrentVcpuHeader>"));
    assert!(header.contains("struct CurrentVcpuHeader"));
    assert!(header.contains("pending_interrupts"));
    assert!(header.contains("fetch_or("));
    assert!(manager.contains("publish_current_vcpu_interrupt(vector)"));
    for forbidden in ["Box::new", "Vec::", "BTreeMap", ".lock()"] {
        let function = manager
            .split_once("pub fn inject_current_vcpu_interrupt")
            .expect("current-vCPU injection API must exist")
            .1
            .split_once("impl AxvmRuntime")
            .expect("current-vCPU injection API must end before runtime impl")
            .0;
        assert!(
            !function.contains(forbidden),
            "hard-IRQ current-vCPU injection must not use {forbidden}"
        );
    }
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

fn assert_in_order(source: &str, patterns: &[&str]) {
    let mut cursor = 0;
    for pattern in patterns {
        let offset = source[cursor..]
            .find(pattern)
            .unwrap_or_else(|| panic!("missing ordered pattern {pattern:?}"));
        cursor += offset + pattern.len();
    }
}
