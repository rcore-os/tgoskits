//! Host callbacks required by the OS-neutral AArch64 vCPU core.

use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

/// Host operations required by AArch64 virtualization code.
///
/// The vCPU core calls these static methods at architecture boundaries where
/// the embedding OS or VMM owns physical interrupt reporting and current-EL
/// interrupt dispatch. Guest interrupt delivery belongs to the registered
/// interrupt controller rather than this host callback.
pub trait ArmHostOps {
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
