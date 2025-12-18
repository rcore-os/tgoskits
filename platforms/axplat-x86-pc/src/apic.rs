//! Advanced Programmable Interrupt Controller (APIC) support.

use core::mem::MaybeUninit;

use axplat::mem::{PhysAddr, pa, phys_to_virt};
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use x2apic::ioapic::IoApic;
use x2apic::lapic::{LocalApic, LocalApicBuilder, xapic_base};
use x86_64::instructions::port::Port;

use self::vectors::*;

pub(super) mod vectors {
    pub const APIC_TIMER_VECTOR: u8 = 0xf0;
    pub const APIC_SPURIOUS_VECTOR: u8 = 0xf1;
    pub const APIC_ERROR_VECTOR: u8 = 0xf2;
}

const IO_APIC_BASE: PhysAddr = pa!(0xFEC0_0000);

static mut LOCAL_APIC: MaybeUninit<LocalApic> = MaybeUninit::uninit();
static mut IS_X2APIC: bool = false;
static IO_APIC: LazyInit<SpinNoIrq<IoApic>> = LazyInit::new();

/// Enables or disables the given IRQ.
#[cfg(feature = "irq")]
pub fn set_enable(vector: usize, enabled: bool) {
    // should not affect LAPIC interrupts
    if vector < APIC_TIMER_VECTOR as _ {
        unsafe {
            if enabled {
                IO_APIC.lock().enable_irq(vector as u8);
            } else {
                IO_APIC.lock().disable_irq(vector as u8);
            }
        }
    }
}

#[cfg(any(feature = "smp", feature = "irq"))]
#[allow(static_mut_refs)]
pub fn local_apic<'a>() -> &'a mut LocalApic {
    // It's safe as `LOCAL_APIC` is initialized in `init_primary`.
    unsafe { LOCAL_APIC.assume_init_mut() }
}

#[cfg(any(feature = "smp", feature = "irq"))]
pub fn raw_apic_id(id_u8: u8) -> u32 {
    if unsafe { IS_X2APIC } {
        id_u8 as u32
    } else {
        (id_u8 as u32) << 24
    }
}

fn cpu_has_x2apic() -> bool {
    match raw_cpuid::CpuId::new().get_feature_info() {
        Some(finfo) => finfo.has_x2apic(),
        None => false,
    }
}

pub fn init_primary() {
    info!("Initialize Local APIC...");

    unsafe {
        // Disable 8259A interrupt controllers
        Port::<u8>::new(0x21).write(0xff);
        Port::<u8>::new(0xA1).write(0xff);
    }

    let mut builder = LocalApicBuilder::new();
    builder
        .timer_vector(APIC_TIMER_VECTOR as _)
        .error_vector(APIC_ERROR_VECTOR as _)
        .spurious_vector(APIC_SPURIOUS_VECTOR as _);

    if cpu_has_x2apic() {
        info!("Using x2APIC.");
        unsafe { IS_X2APIC = true };
    } else {
        info!("Using xAPIC.");
        let base_vaddr = phys_to_virt(pa!(unsafe { xapic_base() } as usize));
        builder.set_xapic_base(base_vaddr.as_usize() as u64);
    }

    let mut lapic = builder.build().unwrap();
    unsafe {
        lapic.enable();
        #[allow(static_mut_refs)]
        LOCAL_APIC.write(lapic);
    }

    info!("Initialize IO APIC...");
    let io_apic = unsafe { IoApic::new(phys_to_virt(IO_APIC_BASE).as_usize() as u64) };
    IO_APIC.init_once(SpinNoIrq::new(io_apic));
}

#[cfg(feature = "smp")]
pub fn init_secondary() {
    unsafe { local_apic().enable() };
}

#[cfg(feature = "irq")]
mod irq_impl {
    use axplat::irq::{HandlerTable, IpiTarget, IrqHandler, IrqIf};

    /// The maximum number of IRQs.
    const MAX_IRQ_COUNT: usize = 256;

    static IRQ_HANDLER_TABLE: HandlerTable<MAX_IRQ_COUNT> = HandlerTable::new();

    struct IrqIfImpl;

    #[impl_plat_interface]
    impl IrqIf for IrqIfImpl {
        /// Enables or disables the given IRQ.
        fn set_enable(vector: usize, enabled: bool) {
            super::set_enable(vector, enabled);
        }

        /// Registers an IRQ handler for the given IRQ.
        ///
        /// It also enables the IRQ if the registration succeeds. It returns `false` if
        /// the registration failed.
        fn register(vector: usize, handler: IrqHandler) -> bool {
            if IRQ_HANDLER_TABLE.register_handler(vector, handler) {
                Self::set_enable(vector, true);
                return true;
            }
            warn!("register handler for IRQ {} failed", vector);
            false
        }

        /// Unregisters the IRQ handler for the given IRQ.
        ///
        /// It also disables the IRQ if the unregistration succeeds. It returns the
        /// existing handler if it is registered, `None` otherwise.
        fn unregister(vector: usize) -> Option<IrqHandler> {
            Self::set_enable(vector, false);
            IRQ_HANDLER_TABLE.unregister_handler(vector)
        }

        /// Handles the IRQ.
        ///
        /// It is called by the common interrupt handler. It should look up in the
        /// IRQ handler table and calls the corresponding handler. If necessary, it
        /// also acknowledges the interrupt controller after handling.
        fn handle(vector: usize) {
            trace!("IRQ {}", vector);
            if !IRQ_HANDLER_TABLE.handle(vector) {
                warn!("Unhandled IRQ {vector}");
            }
            unsafe { super::local_apic().end_of_interrupt() };
        }

        /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
        fn send_ipi(irq_num: usize, target: IpiTarget) {
            match target {
                IpiTarget::Current { cpu_id: _ } => {
                    unsafe {
                        super::local_apic().send_ipi_self(irq_num as _);
                    };
                }
                IpiTarget::Other { cpu_id } => {
                    let apic_id = super::raw_apic_id(cpu_id as u8);
                    unsafe {
                        super::local_apic().send_ipi(irq_num as _, apic_id as _);
                    };
                }
                IpiTarget::AllExceptCurrent {
                    cpu_id: _,
                    cpu_num: _,
                } => {
                    use x2apic::lapic::IpiAllShorthand;
                    unsafe {
                        super::local_apic()
                            .send_ipi_all(irq_num as _, IpiAllShorthand::AllExcludingSelf);
                    };
                }
            }
        }
    }
}
