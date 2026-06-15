#[cfg(feature = "hypervisor")]
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use ax_plat::irq::{IpiTarget, IrqIf, dispatch_irq};
use loongArch64::{
    iocsr::{iocsr_read_w, iocsr_write_w},
    register::{
        ecfg::{self, LineBasedInterrupt},
        ticlr,
    },
};

use crate::config::devices::{EIOINTC_IRQ, IPI_IRQ, TIMER_IRQ};

// TODO: move these modules to a separate crate
mod eiointc;
mod pch_pic;

/// The maximum number of hypervisor-routed IRQs.
#[cfg(feature = "hypervisor")]
pub const MAX_IRQ_COUNT: usize = 256;
const IOCSR_IPI_SEND_CPU_SHIFT: u32 = 16;
const IOCSR_IPI_SEND_BLOCKING: u32 = 1 << 31;

// [Loongson 3A5000 Manual](https://loongson.github.io/LoongArch-Documentation/Loongson-3A5000-usermanual-EN.html)
// See Section 10.2 for details about IPI registers
const IOCSR_IPI_STATUS: usize = 0x1000;
const IOCSR_IPI_ENABLE: usize = 0x1004;
const IOCSR_IPI_CLEAR: usize = 0x100c;
const IOCSR_IPI_SEND: usize = 0x1040;
#[cfg(feature = "hypervisor")]
const IRQ_ROUTE_NONE: usize = 0;
#[cfg(feature = "hypervisor")]
const IRQ_TARGET_NONE: usize = usize::MAX;
#[cfg(feature = "hypervisor")]
const IRQ_TARGET_VM_SHIFT: usize = 32;

#[cfg(feature = "hypervisor")]
static VIRTUAL_IRQ_INJECTOR: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
#[cfg(feature = "hypervisor")]
static GUEST_IRQ_ROUTES: [AtomicUsize; MAX_IRQ_COUNT] =
    [const { AtomicUsize::new(IRQ_ROUTE_NONE) }; MAX_IRQ_COUNT];
#[cfg(feature = "hypervisor")]
static GUEST_IRQ_TARGETS: [AtomicUsize; MAX_IRQ_COUNT] =
    [const { AtomicUsize::new(IRQ_TARGET_NONE) }; MAX_IRQ_COUNT];

fn make_ipi_send_value(cpu_id: usize, vector: u32, blocking: bool) -> u32 {
    let mut value = (cpu_id as u32) << IOCSR_IPI_SEND_CPU_SHIFT | vector;
    if blocking {
        value |= IOCSR_IPI_SEND_BLOCKING;
    }
    value
}

/// Registers the virtual interrupt injector used by hypervisor builds.
#[cfg(feature = "hypervisor")]
pub fn register_virtual_irq_injector(injector: fn(usize, usize, usize, usize)) {
    VIRTUAL_IRQ_INJECTOR.store(injector as *mut (), Ordering::Release);
    info!("LoongArch platform virtual IRQ injector registered");
}

#[cfg(feature = "hypervisor")]
fn encode_guest_vector(vector: usize) -> usize {
    vector + 1
}

#[cfg(feature = "hypervisor")]
fn decode_guest_vector(encoded: usize) -> Option<usize> {
    (encoded != IRQ_ROUTE_NONE).then_some(encoded - 1)
}

#[cfg(feature = "hypervisor")]
fn encode_guest_target(vm_id: usize, vcpu_id: usize) -> usize {
    (vm_id << IRQ_TARGET_VM_SHIFT) | vcpu_id
}

#[cfg(feature = "hypervisor")]
fn decode_guest_target(encoded: usize) -> Option<(usize, usize)> {
    (encoded != IRQ_TARGET_NONE).then_some((
        encoded >> IRQ_TARGET_VM_SHIFT,
        encoded & ((1usize << IRQ_TARGET_VM_SHIFT) - 1),
    ))
}

#[cfg(feature = "hypervisor")]
fn guest_route(physical_irq: usize) -> Option<(usize, usize, usize)> {
    if physical_irq >= MAX_IRQ_COUNT {
        return None;
    }
    let guest_vector = decode_guest_vector(GUEST_IRQ_ROUTES[physical_irq].load(Ordering::Acquire))?;
    let (vm_id, vcpu_id) =
        decode_guest_target(GUEST_IRQ_TARGETS[physical_irq].load(Ordering::Acquire))?;
    Some((vm_id, vcpu_id, guest_vector))
}

/// Routes one physical EIOINTC/PCH-PIC IRQ to a guest CPU interrupt vector.
#[cfg(feature = "hypervisor")]
pub fn register_guest_irq_route(
    physical_irq: usize,
    vm_id: usize,
    vcpu_id: usize,
    guest_vector: usize,
) {
    if physical_irq >= MAX_IRQ_COUNT {
        warn!("LoongArch guest IRQ route ignored: physical IRQ {physical_irq} out of range");
        return;
    }

    GUEST_IRQ_ROUTES[physical_irq].store(encode_guest_vector(guest_vector), Ordering::Release);
    GUEST_IRQ_TARGETS[physical_irq].store(encode_guest_target(vm_id, vcpu_id), Ordering::Release);
    eiointc::enable_irq(physical_irq);
    pch_pic::enable_irq(physical_irq);
    debug!(
        "LoongArch guest IRQ route: physical_irq={}, target=VM[{}] VCpu[{}], guest_vector={}",
        physical_irq, vm_id, vcpu_id, guest_vector
    );
}

pub(crate) fn init() {
    eiointc::init();
    pch_pic::init();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IrqType {
    Timer,
    Ipi,
    Io,
    Ex(usize),
}

impl IrqType {
    fn new(irq: usize) -> Self {
        match irq {
            TIMER_IRQ => Self::Timer,
            IPI_IRQ => Self::Ipi,
            EIOINTC_IRQ => Self::Io,
            n => Self::Ex(n),
        }
    }

    fn as_usize(&self) -> usize {
        match self {
            IrqType::Timer => TIMER_IRQ,
            IrqType::Ipi => IPI_IRQ,
            IrqType::Io => EIOINTC_IRQ,
            IrqType::Ex(n) => *n,
        }
    }

    fn as_line(&self) -> Option<LineBasedInterrupt> {
        match self {
            IrqType::Timer => Some(LineBasedInterrupt::TIMER),
            IrqType::Ipi => Some(LineBasedInterrupt::IPI),
            IrqType::Io => LineBasedInterrupt::from_bits(1 << EIOINTC_IRQ),
            _ => None,
        }
    }
}

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    /// Enables or disables the given IRQ.
    fn set_enable(irq: usize, enabled: bool) {
        let irq = IrqType::new(irq);

        match irq {
            IrqType::Ipi => {
                let value = if enabled { u32::MAX } else { 0 };
                iocsr_write_w(IOCSR_IPI_ENABLE, value);
            }
            IrqType::Ex(irq) => {
                if enabled {
                    eiointc::enable_irq(irq);
                    pch_pic::enable_irq(irq);
                } else {
                    eiointc::disable_irq(irq);
                    pch_pic::disable_irq(irq);
                }
            }
            _ => {}
        }

        if let Some(line) = irq.as_line() {
            let old_value = ecfg::read().lie();
            let new_value = match enabled {
                true => old_value | line,
                false => old_value & !line,
            };
            ecfg::set_lie(new_value);
        }
    }

    fn set_affinity(
        _irq: usize,
        _affinity: ax_plat::irq::IrqAffinity,
    ) -> Result<(), ax_plat::irq::IrqError> {
        Err(ax_plat::irq::IrqError::Unsupported)
    }

    /// Handles the IRQ.
    fn handle(irq: usize) -> Option<usize> {
        let mut irq = IrqType::new(irq);

        if matches!(irq, IrqType::Io) {
            let Some(ex_irq) = eiointc::claim_irq() else {
                debug!("Spurious external IRQ");
                return None;
            };

            #[cfg(feature = "hypervisor")]
            if let Some((vm_id, vcpu_id, guest_vector)) = guest_route(ex_irq) {
                let injector = VIRTUAL_IRQ_INJECTOR.load(Ordering::Acquire);
                if injector.is_null() {
                    warn!(
                        "LoongArch guest-owned IRQ {ex_irq} has no virtual IRQ injector; leaving \
                         it pending for guest"
                    );
                    return Some(ex_irq);
                }

                // SAFETY: The injector is registered through
                // register_virtual_irq_injector as a valid function pointer.
                unsafe {
                    core::mem::transmute::<*mut (), fn(usize, usize, usize, usize)>(injector)(
                        vm_id,
                        vcpu_id,
                        guest_vector,
                        ex_irq,
                    );
                }
                return Some(ex_irq);
            }

            irq = IrqType::Ex(ex_irq);
        }

        trace!("IRQ {irq:?}");

        match irq {
            IrqType::Timer => {
                // Clear the interrupt before dispatching. The timer handler
                // programs the next one-shot event; clearing afterwards can
                // drop a freshly-pending event and leave sleepers blocked.
                ticlr::clear_timer_interrupt();
                if !dispatch_irq(irq.as_usize()).handled {
                    debug!("Unhandled IRQ {irq:?}");
                }
            }
            IrqType::Ipi => {
                let mut status = iocsr_read_w(IOCSR_IPI_STATUS);
                if status != 0 {
                    iocsr_write_w(IOCSR_IPI_CLEAR, status);
                    trace!("IPI status = {:#x}", status);

                    while status != 0 {
                        let vector = status.trailing_zeros() as usize;
                        status &= !(1 << vector);
                        if !dispatch_irq(irq.as_usize()).handled {
                            warn!("Unhandled IRQ {irq:?}");
                        }
                    }
                }
            }
            IrqType::Io | IrqType::Ex(_) => {
                if !dispatch_irq(irq.as_usize()).handled {
                    debug!("Unhandled IRQ {irq:?}");
                }
            }
        }

        if let IrqType::Ex(irq) = irq {
            eiointc::complete_irq(irq);
        }

        Some(irq.as_usize())
    }

    /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
    ///
    /// Runtime IPIs are sent NON-blocking (`IOCSR_IPI_SEND_BLOCKING` unset). The
    /// blocking variant stalls the issuing CPU until the target clears its
    /// `IOCSR_IPI_STATUS`; under a high-rate IPI burst the sender can block while
    /// the target is mid-handler (IRQs disabled) — or while the sender itself holds
    /// an IRQ-disabling lock — which deadlocks (the arceos-ipi SMP test hung 6h on
    /// loongarch). Linux/riscv/x86 likewise fire runtime IPIs non-blocking; the
    /// blocking form is reserved for the secondary-CPU boot mailbox (see `mp.rs`).
    fn send_ipi(_irq_num: usize, target: IpiTarget) {
        match target {
            IpiTarget::Current { cpu_id } => {
                iocsr_write_w(IOCSR_IPI_SEND, make_ipi_send_value(cpu_id, 0, false));
            }
            IpiTarget::Other { cpu_id } => {
                iocsr_write_w(IOCSR_IPI_SEND, make_ipi_send_value(cpu_id, 0, false));
            }
            IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                for i in 0..cpu_num {
                    if i != cpu_id {
                        iocsr_write_w(IOCSR_IPI_SEND, make_ipi_send_value(i, 0, false));
                    }
                }
            }
        }
    }
}

#[cfg(feature = "hypervisor")]
#[impl_plat_interface]
impl ax_plat::irq::LoongArchHvIrqIf for IrqIfImpl {
    fn register_virtual_irq_injector(injector: fn(usize, usize, usize, usize)) {
        register_virtual_irq_injector(injector);
    }

    fn register_guest_irq_route(
        physical_irq: usize,
        vm_id: usize,
        vcpu_id: usize,
        guest_vector: usize,
    ) {
        register_guest_irq_route(physical_irq, vm_id, vcpu_id, guest_vector);
    }
}
