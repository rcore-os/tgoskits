//! Host callbacks required by the OS-neutral AArch64 vCPU core.

use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use ax_cpu_local::CpuPin;

use crate::ArmVcpuResult;

/// Host operations required by AArch64 virtualization code.
///
/// The vCPU core calls these static methods at architecture boundaries where
/// the embedding OS or VMM owns the policy: virtual interrupt injection,
/// physical interrupt reporting, and current-EL interrupt dispatch.
pub trait ArmHostOps {
    /// Inject a virtual interrupt through host interrupt-controller state.
    fn inject_virtual_interrupt(vector: u8) -> ArmVcpuResult;

    /// Claims and dispatches a lower-EL host IRQ after backend unbind.
    ///
    /// # Safety
    ///
    /// The caller must have restored every host CPU/task register, unbound the
    /// vCPU, removed its current-vCPU publication, and remain on the CPU pinned
    /// by `cpu_pin`. Raw IRQs must still be masked by the unique DAIF state
    /// retained from the lower-EL exit. The implementation must claim,
    /// dispatch, and complete at most that one pending host interrupt without
    /// restoring DAIF or retaining the CPU pin.
    unsafe fn handle_post_unbind_host_irq(cpu_pin: &CpuPin) -> ArmVcpuResult;

    /// Dispatch a host IRQ taken while running at the current exception level.
    ///
    /// # Safety
    ///
    /// This callback may only be invoked by the current-EL architecture IRQ
    /// entry while its exception frame owns restoration of the interrupted
    /// DAIF state. Lower-EL VM exits must retain their saved DAIF owner and use
    /// [`Self::handle_post_unbind_host_irq`] after guest state is unbound.
    unsafe fn handle_current_host_irq();
}

static CURRENT_EL_IRQ_HANDLER: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static CURRENT_EL_IRQ_HANDLER_USERS: AtomicUsize = AtomicUsize::new(0);

unsafe fn current_el_irq_handler_for<H: ArmHostOps>() {
    // SAFETY: forwarded from the current-EL architecture entry contract.
    unsafe { H::handle_current_host_irq() };
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

/// Dispatches the current host IRQ through the installed embedding callback.
///
/// # Safety
///
/// The caller must be the current-EL architecture IRQ entry and retain the
/// exception frame which owns restoration of the interrupted DAIF state.
pub(crate) unsafe fn handle_current_host_irq() {
    let handler = CURRENT_EL_IRQ_HANDLER.load(Ordering::Acquire);
    if handler.is_null() {
        panic!("arm_vcpu current-EL IRQ handler is not installed");
    }

    let handler: unsafe fn() = unsafe { core::mem::transmute(handler) };
    // SAFETY: forwarded caller contract.
    unsafe { handler() };
}
