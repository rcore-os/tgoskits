use core::{num::NonZeroU32, ptr::NonNull};

use ax_kspin::SpinNoIrq;
use ax_plat::{
    irq::{IpiTarget, IrqIf, dispatch_irq},
    percpu::this_cpu_id,
};
use ax_riscv_plic::Plic;
use riscv::register::sie;
use sbi_rt::HartMask;

use crate::config::{devices::PLIC_PADDR, plat::PHYS_VIRT_OFFSET};

/// `Interrupt` bit in `scause`
pub(super) const INTC_IRQ_BASE: usize = 1 << (usize::BITS - 1);

/// Supervisor software interrupt in `scause`
#[allow(unused)]
pub(super) const S_SOFT: usize = INTC_IRQ_BASE + 1;

/// Supervisor timer interrupt in `scause`
pub(super) const S_TIMER: usize = INTC_IRQ_BASE + 5;

/// Supervisor external interrupt in `scause`
pub(super) const S_EXT: usize = INTC_IRQ_BASE + 9;

static PLIC: SpinNoIrq<Plic> = SpinNoIrq::new(unsafe {
    Plic::new(NonNull::new((PHYS_VIRT_OFFSET + PLIC_PADDR) as *mut _).unwrap())
});

fn this_context() -> usize {
    let hart_id = this_cpu_id() + 1;
    // hart 0 missing S-mode
    hart_id * 2 // supervisor context
}

pub(super) fn init_percpu() {
    // enable soft interrupts, timer interrupts, and external interrupts
    unsafe {
        sie::set_ssoft();
        sie::set_stimer();
        sie::set_sext();
    }
    PLIC.lock().init_by_context(this_context());
}

macro_rules! with_cause {
    (
        $cause:expr, @S_TIMER =>
        $timer_op:expr, @S_SOFT =>
        $ipi_op:expr, @S_EXT =>
        $ext_op:expr, @EX_IRQ =>
        $plic_op:expr $(,)?
    ) => {
        match $cause {
            S_TIMER => $timer_op,
            S_SOFT => $ipi_op,
            S_EXT => $ext_op,
            other => {
                if other & INTC_IRQ_BASE == 0 {
                    // Device-side interrupts read from PLIC
                    $plic_op
                } else {
                    // Other CPU-side interrupts
                    panic!("Unknown IRQ cause: {}", other);
                }
            }
        }
    };
}

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    /// Enables or disables the given IRQ.
    fn set_enable(irq: usize, enabled: bool) {
        with_cause!(
            irq,
            @S_TIMER => {
                unsafe {
                    if enabled {
                        sie::set_stimer();
                    } else {
                        sie::clear_stimer();
                    }
                }
            },
            @S_SOFT => {},
            @S_EXT => {},
            @EX_IRQ => {
                let Some(irq) = NonZeroU32::new(irq as _) else {
                    return;
                };
                let mut plic = PLIC.lock();
                if enabled {
                    plic.set_priority(irq, 6);
                    plic.enable(irq, this_context());
                } else {
                    plic.disable(irq, this_context());
                }
            }
        );
    }

    fn set_affinity(
        _irq: usize,
        _affinity: ax_plat::irq::IrqAffinity,
    ) -> Result<(), ax_plat::irq::IrqError> {
        Err(ax_plat::irq::IrqError::Unsupported)
    }

    /// Handles the IRQ.
    fn handle(irq: usize) -> Option<usize> {
        with_cause!(
            irq,
            @S_TIMER => {
                trace!("IRQ: timer");
                if !dispatch_irq(irq).handled {
                    warn!("Unhandled IRQ: timer");
                }
                Some(irq)
            },
            @S_SOFT => {
                trace!("IRQ: IPI");
                if !dispatch_irq(irq).handled {
                    warn!("Unhandled IRQ: IPI");
                }
                Some(irq)
            },
            @S_EXT => {
                let mut plic = PLIC.lock();
                let Some(irq) = plic.claim(this_context()) else {
                    debug!("Spurious external IRQ");
                    return None;
                };
                trace!("IRQ: external {irq}");
                drop(plic);
                if !dispatch_irq(irq.get() as usize).handled {
                    debug!("Unhandled external IRQ {irq}");
                }
                PLIC.lock().complete(this_context(), irq);
                Some(irq.get() as usize)
            },
            @EX_IRQ => {
                unreachable!("Device-side IRQs should be handled by triggering the External Interrupt.");
            }
        )
    }

    /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
    fn send_ipi(_irq_num: usize, target: IpiTarget) {
        match target {
            IpiTarget::Current { cpu_id } => {
                let res = sbi_rt::send_ipi(HartMask::from_mask_base(1 << cpu_id, 0));
                if res.is_err() {
                    warn!("send_ipi failed: {res:?}");
                }
            }
            IpiTarget::Other { cpu_id } => {
                let res = sbi_rt::send_ipi(HartMask::from_mask_base(1 << cpu_id, 0));
                if res.is_err() {
                    warn!("send_ipi failed: {res:?}");
                }
            }
            IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                for i in 0..cpu_num {
                    if i != cpu_id {
                        let res = sbi_rt::send_ipi(HartMask::from_mask_base(1 << i, 0));
                        if res.is_err() {
                            warn!("send_ipi_all_others failed: {res:?}");
                        }
                    }
                }
            }
        }
    }
}
