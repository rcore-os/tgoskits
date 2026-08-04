//! Host callbacks and execution guards required by the OS-neutral AArch64
//! vCPU core.

use core::{
    marker::PhantomData,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use crate::ArmVcpuResult;

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

/// Host operations required by AArch64 virtualization code.
///
/// The vCPU core calls these static methods at architecture boundaries where
/// the embedding OS or VMM owns the policy: virtual interrupt injection,
/// physical interrupt reporting, and current-EL interrupt dispatch.
pub trait ArmHostOps {
    /// Inject a virtual interrupt through host interrupt-controller state.
    fn inject_virtual_interrupt(vector: u32) -> ArmVcpuResult;

    /// Completes the priority drop for an IAR value captured by the
    /// assembly-only lower-EL IRQ exit path.
    ///
    /// The implementation must return the stable token used for later
    /// deactivate, or `None` for a special/spurious acknowledgement.
    fn finish_pending_host_irq(raw_ack: u32) -> Option<usize>;

    /// Dispatch a host IRQ taken while running at the current exception level.
    fn handle_current_host_irq();
}

static CURRENT_EL_IRQ_HANDLER: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static CURRENT_EL_IRQ_HANDLER_USERS: AtomicUsize = AtomicUsize::new(0);

fn current_el_irq_handler_for<H: ArmHostOps>() {
    H::handle_current_host_irq();
}

/// Installs the current-EL IRQ handler used by the EL2 exception vector.
///
/// This is intentionally a process-wide hook: an `arm_vcpu` instance is generic
/// over the embedding host, but the assembly vector entered from current EL does
/// not carry that generic type. The VMM installs the hook when enabling EL2
/// virtualization on a CPU.
pub(crate) fn install_current_el_irq_handler<H: ArmHostOps>() {
    let handler = current_el_irq_handler_for::<H> as *mut ();
    match CURRENT_EL_IRQ_HANDLER.compare_exchange(
        core::ptr::null_mut(),
        handler,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => {}
        Err(existing) if existing == handler => {}
        Err(_) => panic!("arm_vcpu current-EL IRQ handler was installed by another host type"),
    }

    CURRENT_EL_IRQ_HANDLER_USERS.fetch_add(1, Ordering::AcqRel);
}

pub(crate) fn clear_current_el_irq_handler() {
    loop {
        let users = CURRENT_EL_IRQ_HANDLER_USERS.load(Ordering::Acquire);
        if users == 0 {
            panic!("arm_vcpu current-EL IRQ handler was not installed");
        }

        if CURRENT_EL_IRQ_HANDLER_USERS
            .compare_exchange(users, users - 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            if users == 1 {
                CURRENT_EL_IRQ_HANDLER.store(core::ptr::null_mut(), Ordering::Release);
            }
            break;
        }
    }
}

pub(crate) fn handle_current_host_irq() {
    let handler = CURRENT_EL_IRQ_HANDLER.load(Ordering::Acquire);
    if handler.is_null() {
        panic!("arm_vcpu current-EL IRQ handler is not installed");
    }

    let handler: fn() = unsafe { core::mem::transmute(handler) };
    handler();
}
