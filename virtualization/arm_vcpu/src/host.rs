//! Host callbacks required by the OS-neutral AArch64 vCPU core.

#[cfg(target_arch = "aarch64")]
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::{ArmVcpuResult, ArmVirtualIntId};

/// Host operations required by AArch64 virtualization code.
///
/// The vCPU core calls these static methods at architecture boundaries where
/// the embedding OS or VMM owns the policy: virtual interrupt injection,
/// physical interrupt reporting, and current-EL interrupt dispatch.
pub trait ArmHostOps {
    /// Inject a virtual interrupt through host interrupt-controller state.
    fn inject_virtual_interrupt(intid: ArmVirtualIntId) -> ArmVcpuResult;

    /// Report a pending host IRQ after a lower-EL IRQ VM exit.
    fn fetch_pending_host_irq() -> Option<usize>;

    /// Dispatch a host IRQ taken while running at the current exception level.
    fn handle_current_host_irq();
}

#[cfg(any(target_arch = "aarch64", test))]
pub(crate) fn inject_virtual_interrupt_for<H: ArmHostOps>(vector: usize) -> ArmVcpuResult {
    H::inject_virtual_interrupt(ArmVirtualIntId::try_from(vector)?)
}

#[cfg(target_arch = "aarch64")]
static CURRENT_EL_IRQ_HANDLER: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
#[cfg(target_arch = "aarch64")]
static CURRENT_EL_IRQ_HANDLER_USERS: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_arch = "aarch64")]
fn current_el_irq_handler_for<H: ArmHostOps>() {
    H::handle_current_host_irq();
}

/// Installs the current-EL IRQ handler used by the EL2 exception vector.
///
/// This is intentionally a process-wide hook: an `arm_vcpu` instance is generic
/// over the embedding host, but the assembly vector entered from current EL does
/// not carry that generic type. The VMM installs the hook when enabling EL2
/// virtualization on a CPU.
#[cfg(target_arch = "aarch64")]
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

#[cfg(target_arch = "aarch64")]
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

#[cfg(target_arch = "aarch64")]
pub(crate) fn handle_current_host_irq() {
    let handler = CURRENT_EL_IRQ_HANDLER.load(Ordering::Acquire);
    if handler.is_null() {
        panic!("arm_vcpu current-EL IRQ handler is not installed");
    }

    let handler: fn() = unsafe { core::mem::transmute(handler) };
    handler();
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static OBSERVED_INTID: AtomicU32 = AtomicU32::new(u32::MAX);

    struct RecordingHost;

    impl ArmHostOps for RecordingHost {
        fn inject_virtual_interrupt(intid: ArmVirtualIntId) -> ArmVcpuResult {
            OBSERVED_INTID.store(intid.as_u32(), Ordering::Relaxed);
            Ok(())
        }

        fn fetch_pending_host_irq() -> Option<usize> {
            None
        }

        fn handle_current_host_irq() {}
    }

    #[test]
    fn host_boundary_preserves_intid_256() {
        inject_virtual_interrupt_for::<RecordingHost>(256).unwrap();
        assert_eq!(OBSERVED_INTID.load(Ordering::Relaxed), 256);
    }
}
