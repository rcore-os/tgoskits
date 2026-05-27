use core::sync::atomic::{AtomicPtr, Ordering};

use ax_plat::irq::{HandlerTable, IrqHandler, IrqIf};
use somehal::irq_handler;

/// The maximum number of IRQs.
const MAX_IRQ_COUNT: usize = 1024;
#[cfg(target_arch = "aarch64")]
const GIC_SPECIAL_IRQ_START: usize = 1020;

#[cfg(target_arch = "riscv64")]
const INTC_IRQ_BASE: usize = 1usize << (usize::BITS as usize - 1);
#[cfg(target_arch = "riscv64")]
const S_SOFT: usize = INTC_IRQ_BASE | 1;
#[cfg(target_arch = "riscv64")]
const S_TIMER: usize = INTC_IRQ_BASE | 5;
#[cfg(target_arch = "riscv64")]
const S_EXT: usize = INTC_IRQ_BASE | 9;

static IRQ_HANDLER_TABLE: HandlerTable<MAX_IRQ_COUNT> = HandlerTable::new();

#[cfg(target_arch = "riscv64")]
static TIMER_HANDLER: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
#[cfg(target_arch = "riscv64")]
static IPI_HANDLER: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static VIRTUAL_IRQ_INJECTOR: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub fn register_virtual_irq_injector(injector: fn(usize)) {
    VIRTUAL_IRQ_INJECTOR.store(injector as *mut (), Ordering::Release);
}

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    /// Enables or disables the given IRQ.
    fn set_enable(irq_raw: usize, enabled: bool) {
        somehal::irq::irq_set_enable(irq_raw.into(), enabled);
    }

    /// Registers an IRQ handler for the given IRQ.
    ///
    /// It also enables the IRQ if the registration succeeds. It returns `false`
    /// if the registration failed.
    fn register(irq_num: usize, handler: IrqHandler) -> bool {
        debug!("register handler IRQ {}", irq_num);

        #[cfg(target_arch = "riscv64")]
        {
            if register_local_irq(irq_num, handler) {
                Self::set_enable(irq_num, true);
                return true;
            }
            if is_riscv_local_irq(irq_num) {
                warn!("register handler for local IRQ {} failed", irq_num);
                return false;
            }
        }

        if IRQ_HANDLER_TABLE.register_handler(irq_num, handler) {
            Self::set_enable(irq_num, true);
            return true;
        }
        warn!("register handler for IRQ {} failed", irq_num);
        false
    }

    /// Unregisters the IRQ handler for the given IRQ.
    ///
    /// It also disables the IRQ if the unregistration succeeds. It returns the
    /// existing handler if it is registered, `None` otherwise.
    fn unregister(irq_num: usize) -> Option<IrqHandler> {
        trace!("unregister handler IRQ {}", irq_num);
        Self::set_enable(irq_num, false);
        #[cfg(target_arch = "riscv64")]
        {
            if let Some(handler) = unregister_local_irq(irq_num) {
                return Some(handler);
            }
            if is_riscv_local_irq(irq_num) {
                return None;
            }
        }
        IRQ_HANDLER_TABLE.unregister_handler(irq_num)
    }

    /// Handles the IRQ.
    ///
    /// It is called by the common interrupt handler. It should look up in the
    /// IRQ handler table and calls the corresponding handler. If necessary, it
    /// also acknowledges the interrupt controller after handling.
    fn handle(irq_num: usize) -> Option<usize> {
        let irq = somehal::irq::irq_handler_with_raw(irq_num)?;
        Some(irq.raw())
    }

    fn send_ipi(id: usize, target: ax_plat::irq::IpiTarget) {
        let target = match target {
            ax_plat::irq::IpiTarget::Current { cpu_id } => {
                somehal::irq::IpiTarget::Current { cpu_id }
            }
            ax_plat::irq::IpiTarget::Other { cpu_id } => somehal::irq::IpiTarget::Other { cpu_id },
            ax_plat::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                somehal::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num }
            }
        };
        somehal::irq::send_ipi(id.into(), target);
    }
}

#[irq_handler]
fn somehal_handle_irq(irq: somehal::irq::IrqId) {
    #[cfg(target_arch = "aarch64")]
    if irq.raw() >= GIC_SPECIAL_IRQ_START {
        trace!("Ignoring special IRQ {irq:?}");
        return;
    }

    #[cfg(target_arch = "riscv64")]
    if handle_riscv_local_irq(irq.raw()) {
        return;
    }

    #[cfg(all(target_arch = "riscv64", feature = "hv"))]
    if inject_virtual_irq(irq.raw()) {
        return;
    }

    if irq.raw() < MAX_IRQ_COUNT && IRQ_HANDLER_TABLE.handle(irq.raw()) {
        return;
    }

    if irq.raw() >= MAX_IRQ_COUNT {
        warn!("IRQ {irq:?} is outside handler table");
    } else {
        warn!("Unhandled IRQ {irq:?}");
    }
}

#[cfg(target_arch = "riscv64")]
fn is_riscv_local_irq(irq: usize) -> bool {
    matches!(irq, S_TIMER | S_SOFT | S_EXT) || irq & INTC_IRQ_BASE != 0
}

#[cfg(target_arch = "riscv64")]
fn register_local_irq(irq: usize, handler: IrqHandler) -> bool {
    let slot = match irq {
        S_TIMER => &TIMER_HANDLER,
        S_SOFT => &IPI_HANDLER,
        S_EXT => return false,
        _ => return false,
    };
    slot.compare_exchange(
        core::ptr::null_mut(),
        handler as *mut (),
        Ordering::AcqRel,
        Ordering::Acquire,
    )
    .is_ok()
}

#[cfg(target_arch = "riscv64")]
fn unregister_local_irq(irq: usize) -> Option<IrqHandler> {
    let slot = match irq {
        S_TIMER => &TIMER_HANDLER,
        S_SOFT => &IPI_HANDLER,
        _ => return None,
    };
    let handler = slot.swap(core::ptr::null_mut(), Ordering::AcqRel);
    if handler.is_null() {
        None
    } else {
        Some(unsafe { core::mem::transmute::<*mut (), IrqHandler>(handler) })
    }
}

#[cfg(target_arch = "riscv64")]
fn handle_riscv_local_irq(irq: usize) -> bool {
    let slot = match irq {
        S_TIMER => &TIMER_HANDLER,
        S_SOFT => &IPI_HANDLER,
        S_EXT => return false,
        _ if irq & INTC_IRQ_BASE != 0 => {
            warn!("Unhandled RISC-V local IRQ {irq:#x}");
            return true;
        }
        _ => return false,
    };
    let handler = slot.load(Ordering::Acquire);
    if handler.is_null() {
        warn!("Unhandled RISC-V local IRQ {irq:#x}");
        return true;
    }
    unsafe {
        core::mem::transmute::<*mut (), IrqHandler>(handler)(irq);
    }
    true
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn inject_virtual_irq(irq: usize) -> bool {
    let injector = VIRTUAL_IRQ_INJECTOR.load(Ordering::Acquire);
    if injector.is_null() {
        warn!("virtual IRQ injector is not registered");
        return false;
    }
    unsafe {
        core::mem::transmute::<*mut (), fn(usize)>(injector)(irq);
    }
    true
}
