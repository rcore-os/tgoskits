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
fn aarch64_timer_route_comes_from_host_and_guest_device_trees() {
    let fdt = include_str!("../src/arch/aarch64/fdt.rs");
    assert!(
        fdt.contains("arm,armv8-timer")
            && fdt.contains("ARCH_TIMER_VIRTUAL_IRQ_INDEX")
            && fdt.contains("set_aarch64_virtual_timer_irq"),
        "AArch64 must derive the guest virtual-timer PPI from the selected guest FDT"
    );
    let capabilities = include_str!("../src/arch/aarch64/capabilities.rs");
    assert!(
        capabilities
            .contains("super::fdt::handle_fdt_operations(vm_config, vm_create_config, provider)"),
        "the active AArch64 GuestBootPlatform path must retain the derived timer route"
    );

    let gic = include_str!("../src/arch/aarch64/gic.rs");
    assert!(
        gic.contains("try_get_host_fdt")
            && gic.contains("aarch64_virtual_timer_irq_from_fdt")
            && gic.contains("HOST_VIRTUAL_TIMER_IRQ"),
        "AArch64 must derive the physical CNTV PPI from the host FDT"
    );
}

#[test]
fn aarch64_timer_forwarding_uses_a_hardware_list_register() {
    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    assert!(
        adapter.contains("fn register_platform_irq_injector")
            && adapter.contains("register_guest_virtual_timer_irq_injector"),
        "the AArch64 runtime must install its timer injector during platform setup"
    );
    let first_run = adapter
        .split_once("fn before_first_run")
        .expect("AArch64 must prepare the interrupt interface before guest entry")
        .1
        .split_once("fn handle_vcpu_exit_bound")
        .expect("AArch64 first-run preparation must precede exit handling")
        .0;
    assert!(
        first_run.contains("VMInterruptMode::Emulated")
            && first_run.contains("prepare_emulated_guest_cpu_interface"),
        "the virtual CPU interface must be enabled before an emulated-IRQ guest initializes ICC \
         state"
    );

    let gic = include_str!("../src/arch/aarch64/gic.rs");
    let forwarding = gic
        .split_once("fn forward_current_guest_timer_irq")
        .expect("AArch64 must expose a current-vCPU timer forwarding callback")
        .1;
    assert!(forwarding.contains("VMInterruptMode::Emulated"));
    assert!(forwarding.contains("aarch64_virtual_timer_irq"));
    assert!(forwarding.contains("inject_hardware_interrupt"));
    assert!(
        gic.contains("ICH_LR_EL2::HW::SET") && gic.contains("ICH_LR_EL2::PINTID"),
        "the virtual timer must retain its physical PPI ownership in a hardware LR"
    );
}

#[test]
fn forwarded_timer_defers_physical_deactivation_until_guest_eoi() {
    let platform_irq = include_str!("../../../platforms/axplat-dyn/src/irq.rs");
    assert!(
        platform_irq.contains("inject_aarch64_hardware_irq")
            && platform_irq.contains("defer_deactivation_for_hardware_vint"),
        "the platform IRQ path must transfer an injected physical interrupt to the vCPU interface"
    );

    let gic_v3 = include_str!("../../../platforms/somehal/src/arch/aarch64/gic/v3.rs");
    let drop_impl = gic_v3
        .split_once("impl Drop for ActiveIrq")
        .expect("GICv3 ActiveIrq must own completion")
        .1
        .split_once("pub fn begin_irq")
        .expect("GICv3 begin_irq must follow ActiveIrq completion")
        .0;
    assert!(
        drop_impl.contains("deactivate_on_drop") && drop_impl.contains("dir(self.ack)"),
        "normal IRQs must deactivate on drop while hardware virtual IRQs retain active ownership"
    );
}

#[test]
fn current_vcpu_irq_injection_does_not_reenter_the_rdrive_gic_lock() {
    let gic = include_str!("../src/arch/aarch64/gic.rs");
    let software_injection = gic
        .split_once("pub(crate) fn inject_interrupt(irq: usize)")
        .expect("AArch64 must expose software virtual interrupt injection")
        .1
        .split_once("pub(crate) fn register_guest_virtual_timer_irq_injector")
        .expect("timer injector registration must follow software injection")
        .0;
    assert!(
        !software_injection.contains("with_gic("),
        "software LR injection runs with a current vCPU and must not hold the rdrive GIC lock; \
         the local timer IRQ can preempt it and recursively inject a hardware LR"
    );

    let hardware_injection = gic
        .split_once("fn inject_hardware_interrupt")
        .expect("AArch64 must expose hardware-backed interrupt injection")
        .1
        .split_once("fn inject_interrupt_gic_v3")
        .expect("GICv3 software injection must follow hardware injection")
        .0;
    assert!(
        !hardware_injection.contains("with_gic("),
        "hard-IRQ timer forwarding must use pre-published IRQ-side state instead of locking an \
         rdrive device"
    );
}

#[test]
fn current_vcpu_covers_deferred_hardware_irq_completion() {
    let operations = include_str!("../src/architecture/ops.rs");
    let run_vcpu = operations
        .split_once("fn run_vcpu(")
        .expect("the architecture contract must own the vCPU run loop")
        .1
        .split_once("fn run_bound_vcpu(")
        .expect("the current-vCPU scope must have an explicit bound-run helper")
        .0;
    assert!(
        run_vcpu.contains("vcpu.with_current_cpu_set(||")
            && run_vcpu.contains("Self::run_bound_vcpu(vm, vcpu)"),
        "the current vCPU must stay installed for the complete bound run"
    );

    let bound_run = operations
        .split_once("fn run_bound_vcpu(")
        .expect("the current-vCPU scope must have an explicit bound-run helper")
        .1;
    assert!(
        bound_run.contains("Self::finish_deferred_run_work(vm, vcpu, work)"),
        "deferred host IRQ handling must run before the current vCPU is cleared"
    );
}

#[test]
fn every_vm_exit_quiesces_saved_guest_timers_before_host_scheduling() {
    let arm_vcpu = include_str!("../../arm_vcpu/src/vcpu.rs");
    let run = arm_vcpu
        .split_once("pub fn run(&mut self)")
        .expect("arm_vcpu must expose the guest run boundary")
        .1
        .split_once("pub fn bind(&mut self)")
        .expect("the guest run boundary must remain bounded")
        .0;
    let handle_exit = run
        .find("self.vmexit_handler(trap_kind)")
        .expect("the VM-exit path must acknowledge and route physical IRQs");
    let quiesce = run
        .find("disable_local_guest_timers()")
        .expect("the VM-exit path must quiesce guest timer sources");
    let restore_host_daif = run
        .find("msr daif, {saved_daif}")
        .expect("the VM-exit path must restore host interrupt state");
    assert!(
        handle_exit < quiesce && quiesce < restore_host_daif,
        "the timer source must be disabled after IRQ routing and before host scheduling resumes"
    );

    let vmexit_handler = arm_vcpu
        .split_once("fn vmexit_handler")
        .expect("arm_vcpu must save architecture state on every VM exit")
        .1
        .split_once("fn builtin_sysreg_access_handler")
        .expect("the VM-exit handler must remain bounded")
        .0;
    assert!(vmexit_handler.contains("self.guest_system_regs.store()"));

    let context = include_str!("../../arm_vcpu/src/context_frame.rs");
    let restore = context
        .split_once("pub unsafe fn restore(&self)")
        .expect("arm_vcpu must restore saved guest system registers")
        .1;
    assert!(
        restore.contains("msr CNTV_CVAL_EL0")
            && restore.contains("msr CNTV_CTL_EL0")
            && restore.contains("timer.cntv_ctl_el0"),
        "the next guest entry must restore the saved virtual-timer deadline and control state"
    );
}

#[test]
fn stale_timer_ppi_is_counted_and_quiesced_without_hot_path_output() {
    let gic = include_str!("../src/arch/aarch64/gic.rs");
    let no_current_vcpu = gic
        .split_once("let Some(vcpu) = get_current_vcpu")
        .expect("timer forwarding must identify the current vCPU")
        .1
        .split_once("let Some(vm) = crate::get_vm_by_id")
        .expect("current-vCPU validation must precede VM lookup")
        .0;
    assert!(
        no_current_vcpu.contains("record_unowned_virtual_timer_irq")
            && no_current_vcpu.contains("disable_local_guest_timers"),
        "a stale guest timer PPI must be counted and its per-CPU source quiesced"
    );
    assert!(
        !no_current_vcpu.contains("warn!") && !no_current_vcpu.contains("println!"),
        "the stale-timer hard-IRQ path must not synchronously print"
    );

    let platform_irq = include_str!("../../../platforms/axplat-dyn/src/irq.rs");
    assert!(
        platform_irq.contains("should_log_unhandled_irq")
            && platform_irq.contains("unhandled_count"),
        "unexpected IRQ diagnostics must be bounded instead of printing once per interrupt"
    );
}

#[test]
fn permanently_stopped_vcpu_disables_local_guest_timers_before_unbind() {
    let operations = include_str!("../src/architecture/ops.rs");
    let bound_run = operations
        .split_once("fn run_bound_vcpu(")
        .expect("the architecture contract must own the bound vCPU run loop")
        .1;
    let stop = bound_run
        .find("Self::before_vcpu_stop(vm, vcpu)")
        .expect("permanent vCPU stop must run architecture-local cleanup");
    let unbind = bound_run
        .find("vcpu.unbind()")
        .expect("the bound run loop must unbind the vCPU");
    assert!(
        stop < unbind,
        "guest-owned per-CPU hardware must be quiesced before the vCPU is unbound"
    );

    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    let stop_hook = adapter
        .split_once("fn before_vcpu_stop")
        .expect("AArch64 must implement permanent-stop cleanup")
        .1
        .split_once("fn handle_vcpu_exit_bound")
        .expect("AArch64 stop cleanup must precede exit handling")
        .0;
    assert!(
        stop_hook.contains("arm_vcpu::disable_local_guest_timers()"),
        "AArch64 must remove the stopped guest's timer interrupt sources from the physical CPU"
    );

    let arm_vcpu = include_str!("../../arm_vcpu/src/vcpu.rs");
    let disable = arm_vcpu
        .split_once("pub fn disable_local_guest_timers()")
        .expect("arm_vcpu must expose local guest timer cleanup")
        .1;
    assert!(
        disable.contains("msr CNTP_CTL_EL0, xzr")
            && disable.contains("msr CNTV_CTL_EL0, xzr")
            && disable.contains("isb"),
        "timer cleanup must disable both guest timer sources before returning to host scheduling"
    );
    let arm_vcpu_root = include_str!("../../arm_vcpu/src/lib.rs");
    assert!(
        arm_vcpu_root.contains("disable_local_guest_timers"),
        "arm_vcpu must export timer cleanup to its AxVM adapter"
    );
}

#[test]
fn every_stopping_vcpu_task_quiesces_its_local_guest_timers() {
    let runtime = include_str!("../src/runtime/vcpus.rs");
    let stopping = runtime
        .split_once("if vm.stopping()")
        .expect("the vCPU runtime must handle VM-wide stop publication")
        .1
        .split_once("break;")
        .expect("the stopping branch must terminate the vCPU task")
        .0;
    let quiesce = stopping
        .find("CurrentArch::before_vcpu_task_exit(&vm, &vcpu)")
        .expect("every stopping vCPU task must run architecture-local cleanup");
    let mark_exiting = stopping
        .find("runtime.mark_vcpu_exiting()")
        .expect("the runtime must account for every exiting vCPU");
    assert!(
        quiesce < mark_exiting,
        "local guest hardware must be quiesced before the vCPU is reported as exited"
    );

    let adapter = include_str!("../src/arch/aarch64/mod.rs");
    let task_exit_hook = adapter
        .split_once("fn before_vcpu_task_exit")
        .expect("AArch64 must clean up every stopping vCPU task")
        .1
        .split_once("fn handle_vcpu_exit_bound")
        .expect("AArch64 task-exit cleanup must precede exit handling")
        .0;
    assert!(
        task_exit_hook.contains("arm_vcpu::disable_local_guest_timers()"),
        "secondary vCPUs stopped by another vCPU must disable their local timer sources"
    );
}
