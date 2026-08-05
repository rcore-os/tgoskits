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
fn aarch64_timer_route_comes_from_the_host_fdt_and_reaches_each_vcpu() {
    let timer = include_str!("../src/boot/fdt/core/timer.rs");
    assert!(timer.contains("const TIMER_COMPATIBLE: &str = \"arm,armv8-timer\""));
    let profile = between(timer, "Ok(Some(GuestTimerProfile {", "}))");
    for field in [
        "nonsecure_physical_intid: intids[1]",
        "virtual_intid: intids[2]",
        "clock_frequency_hz",
    ] {
        assert!(
            profile.contains(field),
            "host timer profile is missing `{field}`"
        );
    }

    let core = include_str!("../src/boot/fdt/core/mod.rs");
    let parse = core
        .find("timer::host_timer_profile(&host_fdt)")
        .expect("machine discovery must parse the host timer profile");
    let replace = core
        .find("vm_config.replace_machine_timer(timer)")
        .expect("machine discovery must retain the parsed timer profile");
    assert!(parse < replace);

    let vgic = include_str!("../src/arch/aarch64/vgic.rs");
    let attach = between(
        vgic,
        "pub(crate) fn attach_vcpu(",
        "/// Claims host sources",
    );
    for route in [
        "timer_profile.virtual_intid",
        "timer_profile.nonsecure_physical_intid",
        "TriggerMode::Level",
        "host_virtual_timer_intid: timer_profile.virtual_intid",
    ] {
        assert!(
            attach.contains(route),
            "vCPU timer route is missing `{route}`"
        );
    }
}

#[test]
fn aarch64_timer_forwarding_uses_vm_local_vgic_state() {
    let vgic = include_str!("../src/arch/aarch64/vgic.rs");
    let activation = between(
        vgic,
        "pub(crate) fn activate(&self)",
        "pub(crate) fn deactivate",
    );
    assert!(activation.contains("vtimer::ensure_host_timer_ppi"));

    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    let deferred = between(
        adapter,
        "fn finish_deferred_run_work(",
        "fn wait_for_vcpu_event(",
    );
    assert!(deferred.contains("accept_host_timer_irq(token)"));

    let state = include_str!("../src/arch/aarch64/vtimer/state.rs");
    let accept = between(
        state,
        "fn accept_host_irq(&self",
        "/// Publishes the current timer",
    );
    assert!(accept.contains("host_activation.lock()"));
    assert!(accept.contains("owner_cpu: default_host().this_cpu_id()"));
    let publish = between(
        state,
        "fn publish_levels(",
        "pub(in crate::arch::aarch64) fn retire_host_activation",
    );
    assert!(publish.contains("controller.set_ppi_level"));

    let run = between(adapter, "fn run(&mut self)", "fn bind(&mut self)");
    let load = run.find("binding.load()").unwrap();
    let guest = run.find("self.inner.run(&host_irq_guard)").unwrap();
    let synchronize = run.find("self.synchronize_timer()").unwrap();
    let save = run.find("binding.save()").unwrap();
    assert!(load < guest && guest < synchronize && synchronize < save);
}

#[test]
fn fresh_emulated_vcpu_owns_a_fresh_saved_cpu_interface() {
    let redistributor = include_str!("../../arm_vgic/src/redistributor/mod.rs");
    assert!(
        redistributor.contains("cpu_interface: CpuInterfaceState::new(list_register_count)"),
        "each vCPU redistributor must begin with fresh saved ICH state"
    );

    let binding = include_str!("../../arm_vgic/src/controller/binding.rs");
    let load = between(binding, "pub fn load(&self)", "/// Saves ICH state");
    assert!(load.contains("refill_cpu_interface(self.vcpu)"));
    assert!(load.contains("load_cpu_interface(self.vcpu, &state)"));
    let save = between(binding, "pub fn save(&self)", "/// Harvests completed LRs");
    assert!(save.contains("save_cpu_interface(self.vcpu, &mut saved)"));
    assert!(save.contains("merge_saved_state(saved, false)"));

    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    let create = between(
        adapter,
        "fn new(vm_id: VMId, vcpu_id: VCpuId",
        "fn set_entry(&mut self",
    );
    for fresh in ["vgic: None", "vgic_binding: None", "timer_binding: None"] {
        assert!(
            create.contains(fresh),
            "new vCPU state is missing `{fresh}`"
        );
    }
}

#[test]
fn forwarded_timer_defers_physical_deactivation_until_guest_eoi() {
    let cpu_interface = include_str!("../src/arch/aarch64/gic/cpu_interface.rs");
    let acknowledge = between(
        cpu_interface,
        "fn finish_pending_host_irq_with(",
        "pub(super) fn deactivate_host_irq(",
    );
    assert!(acknowledge.contains("trap.eoi(ack)"));
    assert!(acknowledge.contains("arm_gic_driver::v3::eoi1(ack)"));
    assert!(
        !acknowledge.contains("dir("),
        "host acknowledgement must priority-drop without early deactivation"
    );

    let state = include_str!("../src/arch/aarch64/vtimer/state.rs");
    let accept = between(
        state,
        "fn accept_host_irq(&self",
        "/// Publishes the current timer",
    );
    assert!(accept.contains("*active = Some(activation)"));
    let retire = between(
        state,
        "fn retire_host_activation(&self)",
        "fn complete_host_activation(&self",
    );
    assert!(retire.contains("complete_host_activation(activation)"));

    let backend = include_str!("../src/arch/aarch64/gic.rs");
    let retirement = between(
        backend,
        "fn retire_emulated_interrupt(",
        "fn bind_physical_interrupt(",
    );
    assert!(retirement.contains("binding.retire_host_activation()"));

    let vgic_binding = include_str!("../../arm_vgic/src/controller/binding.rs");
    let retirements = vgic_binding
        .split_once("fn apply_retirements(")
        .expect("VGIC binding must apply saved-interface retirements")
        .1;
    assert!(retirements.contains("retire_emulated_interrupt(self.vcpu, intid)"));
}

#[test]
fn current_vcpu_irq_injection_does_not_reenter_the_rdrive_gic_lock() {
    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    let injection = between(
        adapter,
        "fn inject_interrupt_with_trigger(",
        "fn set_return_value(&mut self",
    );
    assert!(injection.contains("vgic.inject(binding.vcpu().raw(), vector, trigger)"));
    assert!(!injection.contains("try_with_gic"));

    let state = include_str!("../src/arch/aarch64/vtimer/state.rs");
    let forwarding = between(
        state,
        "fn accept_host_irq(&self",
        "/// Publishes the current timer",
    );
    assert!(!forwarding.contains("try_with_gic"));
    assert!(!forwarding.contains("rdrive"));

    let backend = include_str!("../src/arch/aarch64/gic.rs");
    assert!(backend.contains("vCPU load/save and IRQ acknowledge/deactivate use the"));
    assert!(backend.contains("cached CPU-interface capability"));
}

#[test]
fn deferred_host_irq_completion_stays_on_the_acknowledging_pcpu() {
    let runtime = include_str!("../src/runtime/vcpus.rs");
    let run_transaction = between(
        runtime,
        "let run_result = {",
        "#[cfg(feature = \"rt-trace\")]",
    );
    let pin = run_transaction
        .find("NoPreempt::new()")
        .expect("vCPU transaction must pin its pCPU");
    let run = run_transaction
        .find("CurrentArch::run_vcpu(&vm, &vcpu)")
        .expect("runtime must execute the architecture transaction");
    assert!(pin < run);

    let operations = include_str!("../src/architecture/ops.rs");
    let run_vcpu = between(operations, "fn run_vcpu(", "}\n}");
    assert!(run_vcpu.contains("vcpu.with_current_cpu_set(||"));
    let unbind = run_vcpu
        .rfind("let unbind_result = vcpu.unbind()")
        .expect("bound vCPU must be unpublished before deferred work");
    let deferred = run_vcpu
        .rfind("Self::finish_deferred_run_work(vm, vcpu, work)")
        .expect("deferred IRQ token must be completed");
    assert!(unbind < deferred);
}

#[test]
fn every_vm_exit_quiesces_guest_timer_hardware_before_host_scheduling() {
    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    let run = between(adapter, "fn run(&mut self)", "fn bind(&mut self)");
    let mask = run.find("ArmHostIrqGuard::mask()").unwrap();
    let load = run.find("binding.load()").unwrap();
    let guest = run.find("self.inner.run(&host_irq_guard)").unwrap();
    let synchronize = run.find("self.synchronize_timer()").unwrap();
    let save = run.find("binding.save()").unwrap();
    let unmask = run.find("drop(host_irq_guard)").unwrap();
    assert!(mask < load && load < guest && guest < synchronize);
    assert!(synchronize < save && save < unmask);

    let assembly = include_str!("../../arm_vcpu/src/architecture/exception.S");
    let exit = between(
        assembly,
        ".macro SAVE_VCPU_RUNTIME_FROM_EL1",
        ".macro SAVE_VCPU_REGS_FROM_EL1",
    );
    let capture = exit.find("mrs     x9, cntv_ctl_el0").unwrap();
    let stop = exit.find("msr     cntv_ctl_el0, xzr").unwrap();
    let restore_counter = exit.find("msr     cntvoff_el2, xzr").unwrap();
    let publish_unloaded = exit.find("timer_loaded_offset").unwrap();
    assert!(capture < stop && stop < restore_counter && restore_counter < publish_unloaded);
    assert!(exit.contains("msr     cnthctl_el2, x9"));
    assert!(exit.contains("msr     cntkctl_el1, x9"));
    assert!(exit.matches("isb").count() >= 2);
}

#[test]
fn stale_timer_ppi_is_counted_without_hot_path_output() {
    let host_ppi = include_str!("../src/arch/aarch64/vtimer/host_ppi.rs");
    assert!(host_ppi.contains("static HOST_TIMER_PPI: Once<HostTimerPpiClaim>"));
    let fallback = between(
        host_ppi,
        "fn host_timer_ppi_fallback(",
        "fn host_irq_error(",
    );
    assert!(fallback.contains("record_unowned_virtual_timer_irq()"));
    assert!(fallback.contains("irq::IrqReturn::Handled"));
    for forbidden in ["warn!", "println!", "format!", "Vec"] {
        assert!(
            !fallback.contains(forbidden),
            "stale timer PPI fallback contains hot-path operation `{forbidden}`"
        );
    }
}

#[test]
fn permanently_stopped_vcpu_retires_timer_lines_and_host_activation() {
    let state = include_str!("../src/arch/aarch64/vtimer/state.rs");
    let reset = between(state, "fn reset(&self)", "fn publish_levels(");
    assert!(reset.contains("set_ppi_level(self.vcpu, self.virtual_ppi, false)"));
    assert!(reset.contains("set_ppi_level(self.vcpu, self.physical_ppi, false)"));
    assert!(reset.contains("self.retire_host_activation()"));

    let drop_binding = between(state, "impl Drop for Aarch64TimerBinding", "/// # Safety");
    assert!(drop_binding.contains("unregister_timer_ppi"));
    assert!(drop_binding.contains("self.invalidate_wait()"));
    assert!(drop_binding.contains("self.complete_host_activation(activation)"));

    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    let drop_vcpu = between(
        adapter,
        "impl Drop for AxvmArmVcpu",
        "pub(crate) struct AxvmArmPerCpu",
    );
    assert!(drop_vcpu.contains("binding.reset()"));
}

#[test]
fn every_stopping_vcpu_task_quiesces_local_state_before_exit_accounting() {
    let runtime = include_str!("../src/runtime/vcpus.rs");
    let stopping = between(runtime, "if vm.stopping()", "break;");
    let quiesce = stopping
        .find("CurrentArch::before_vcpu_task_exit(&vm, &vcpu)")
        .expect("every stopping vCPU task must run architecture-local cleanup");
    let mark_exiting = stopping
        .find("runtime.mark_vcpu_exiting()")
        .expect("runtime must account for every exiting vCPU");
    assert!(quiesce < mark_exiting);

    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    let task_exit = between(
        adapter,
        "fn before_vcpu_task_exit(",
        "fn handle_vcpu_exit_bound(",
    );
    assert!(task_exit.contains("vm.quiesce_local_reset_memory_cache()"));

    let world_switch = include_str!("../../arm_vcpu/src/architecture/exception.S");
    let exit = between(
        world_switch,
        ".macro SAVE_VCPU_RUNTIME_FROM_EL1",
        ".macro SAVE_VCPU_REGS_FROM_EL1",
    );
    assert!(exit.contains("msr     cntv_ctl_el0, xzr"));
    assert!(exit.contains("strb    wzr, [sp, {timer_loaded_offset}]"));
}
