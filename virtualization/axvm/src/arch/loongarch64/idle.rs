//! LoongArch guest idle handling while host interrupts remain available.

use super::AxvmLoongArchVcpu;

pub(crate) fn wait(vcpu: &crate::vm::AxVCpuRef<AxvmLoongArchVcpu>) {
    crate::check_timer_events();
    if vcpu.get_arch_vcpu().has_enabled_pending_interrupt() {
        trace!(
            "VM[{}] VCpu[{}] skips idle wait because guest has enabled pending interrupt",
            vcpu.vm_id(),
            vcpu.id()
        );
        return;
    }
    let idle_timeout = vcpu.get_arch_vcpu().idle_wait_timeout();
    trace!(
        "VM[{}] VCpu[{}] host idle wait for {idle_timeout:?}",
        vcpu.vm_id(),
        vcpu.id()
    );
    ax_std::os::arceos::modules::ax_hal::asm::set_timer_irq_enabled(true);
    ax_std::os::arceos::modules::ax_hal::asm::enable_irqs();
    ax_std::os::arceos::modules::ax_hal::time::busy_wait(idle_timeout);
    ax_std::os::arceos::modules::ax_hal::asm::disable_irqs();
    ax_std::os::arceos::modules::ax_hal::asm::set_timer_irq_enabled(false);
}
