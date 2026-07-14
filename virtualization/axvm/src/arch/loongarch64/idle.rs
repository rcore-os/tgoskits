//! LoongArch guest idle handling while host interrupts remain available.

use super::AxvmLoongArchVcpu;

pub(crate) fn wait(vcpu: &crate::vm::AxVCpuRef<AxvmLoongArchVcpu>) {
    let has_pending_interrupt = vcpu
        .with_arch_vcpu("check LoongArch pending interrupt", |arch_vcpu| {
            arch_vcpu.has_enabled_pending_interrupt()
        })
        .expect("LoongArch idle handling requires a free vCPU backend");
    if has_pending_interrupt {
        trace!(
            "VM[{}] VCpu[{}] skips idle wait because guest has enabled pending interrupt",
            vcpu.vm_id(),
            vcpu.id()
        );
        return;
    }
    let idle_timeout = vcpu
        .with_arch_vcpu("read LoongArch idle timeout", |arch_vcpu| {
            arch_vcpu.idle_wait_timeout()
        })
        .expect("LoongArch idle handling requires a free vCPU backend");
    trace!(
        "VM[{}] VCpu[{}] host idle wait for {idle_timeout:?}",
        vcpu.vm_id(),
        vcpu.id()
    );
    ax_std::thread::sleep(idle_timeout);
}
