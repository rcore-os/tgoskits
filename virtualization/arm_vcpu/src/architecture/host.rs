//! Host callbacks and execution guards required by the OS-neutral AArch64
//! vCPU core.

use core::marker::PhantomData;

pub use crate::types::ArmHostOps;
use crate::{
    ArmHostPageFaultAccess, ArmVcpuResult,
    enable::{HostHookRegistry, HostHookSet},
};

pub(crate) const HOST_IRQ_INTERFACE_GICV2_MMIO: u64 = 1;
pub(crate) const HOST_IRQ_INTERFACE_GICV3_SYSREG: u64 = 2;

/// Host interrupt-controller interface used by the assembly-only IRQ-exit
/// transaction.
///
/// A lower-EL IRQ must be acknowledged while a level-triggered guest timer is
/// still asserted. The exception vector consumes this immutable configuration
/// to read the host IAR before stopping `CNTV`; Rust only completes the
/// priority drop after the host timer state has been restored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmHostIrqConfig {
    interface: u64,
    cpu_interface_base: usize,
}

impl ArmHostIrqConfig {
    /// Uses the memory-mapped GICv2 CPU interface at `cpu_interface_base`.
    pub const fn gicv2_mmio(cpu_interface_base: usize) -> ArmVcpuResult<Self> {
        if cpu_interface_base == 0
            || !cpu_interface_base.is_multiple_of(core::mem::align_of::<u32>())
        {
            return Err(crate::ArmVcpuError::InvalidInput);
        }
        Ok(Self {
            interface: HOST_IRQ_INTERFACE_GICV2_MMIO,
            cpu_interface_base,
        })
    }

    /// Uses the GICv3 system-register CPU interface.
    pub const fn gicv3_sysreg() -> Self {
        Self {
            interface: HOST_IRQ_INTERFACE_GICV3_SYSREG,
            cpu_interface_base: 0,
        }
    }

    pub(crate) const fn interface(self) -> u64 {
        self.interface
    }

    pub(crate) const fn cpu_interface_base(self) -> usize {
        self.cpu_interface_base
    }
}

/// Proof that host IRQ delivery is masked on the current physical CPU.
///
/// VGIC CPU-interface state is live in hardware from load until save. Keeping
/// host IRQs masked across that complete transaction prevents a current-EL
/// interrupt from publishing canonical VGIC state that a later save would
/// overwrite with the older hardware snapshot.
///
/// The guard restores the previous IRQ mask on drop and is deliberately not
/// transferable between CPUs.
#[must_use = "dropping the guard restores the previous host IRQ mask"]
pub struct ArmHostIrqGuard {
    restore_unmasked: bool,
    _not_send: PhantomData<*mut ()>,
}

impl ArmHostIrqGuard {
    const DAIF_IRQ_MASK: u64 = 1 << 7;

    /// Masks IRQ delivery and records the prior current-CPU state.
    pub fn mask() -> Self {
        let previous_daif: u64;
        // SAFETY: these system-register operations affect only IRQ masking on
        // the current CPU. Omitting `nomem` makes the assembly a compiler
        // barrier for the VGIC load/save transaction protected by this guard.
        unsafe {
            core::arch::asm!(
                "mrs {previous_daif}, daif",
                "msr daifset, #2",
                previous_daif = out(reg) previous_daif,
                options(nostack, preserves_flags)
            );
        }
        Self {
            restore_unmasked: previous_daif & Self::DAIF_IRQ_MASK == 0,
            _not_send: PhantomData,
        }
    }
}

impl Drop for ArmHostIrqGuard {
    fn drop(&mut self) {
        if !self.restore_unmasked {
            return;
        }
        // SAFETY: this guard is dropped on the CPU where it was created and
        // only reverses the IRQ mask transition performed by `mask`.
        unsafe {
            core::arch::asm!("msr daifclr, #2", options(nostack, preserves_flags));
        }
    }
}

static HOST_HOOKS: HostHookRegistry = HostHookRegistry::new();

fn current_el_irq_handler_for<H: ArmHostOps>() {
    H::handle_current_host_irq();
}

fn current_el_sync_handler_for<H: ArmHostOps>(
    saved_pc: &mut usize,
    fault_addr: usize,
    access: ArmHostPageFaultAccess,
    parent_irqs_enabled: bool,
) -> bool {
    H::handle_current_host_page_fault(saved_pc, fault_addr, access, parent_irqs_enabled)
}

fn hook_set_for<H: ArmHostOps>() -> HostHookSet {
    HostHookSet::new(
        current_el_irq_handler_for::<H>,
        current_el_sync_handler_for::<H>,
    )
}

/// Installs the current-EL handlers used by the EL2 exception vector.
///
/// This is intentionally a process-wide hook: an `arm_vcpu` instance is generic
/// over the embedding host, but the assembly vector entered from current EL does
/// not carry that generic type. The VMM installs the hook when enabling EL2
/// virtualization on a CPU.
pub(crate) fn install_host_hooks<H: ArmHostOps>() -> ArmVcpuResult<HostHookSet> {
    let hooks = hook_set_for::<H>();
    HOST_HOOKS.install(hooks)?;
    Ok(hooks)
}

pub(crate) fn validate_host_hook_release(hooks: HostHookSet) -> ArmVcpuResult {
    HOST_HOOKS.validate_release(hooks)
}

pub(crate) fn release_host_hooks(hooks: HostHookSet) {
    HOST_HOOKS.release_validated(hooks);
}

pub(crate) fn handle_current_host_irq() {
    let handler = HOST_HOOKS
        .irq()
        .unwrap_or_else(|| panic!("arm_vcpu current-EL IRQ handler is not installed"));
    handler();
}

pub(crate) fn handle_current_host_page_fault(
    saved_pc: &mut usize,
    fault_addr: usize,
    access: ArmHostPageFaultAccess,
    parent_irqs_enabled: bool,
) -> bool {
    let handler = HOST_HOOKS.synchronous_fault().unwrap_or_else(|| {
        panic!("arm_vcpu current-EL synchronous-fault handler is not installed")
    });
    handler(saved_pc, fault_addr, access, parent_irqs_enabled)
}
