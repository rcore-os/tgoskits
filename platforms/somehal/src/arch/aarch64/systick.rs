use alloc::vec::Vec;

use rdif_intc::Intc;
use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo};

static mut TIMER_IRQ: Option<rdrive::IrqId> = None;
static mut TIMER_IRQ_PARENT: Option<rdrive::DeviceId> = None;
static TIMER_IRQ_VEC: spin::Once<Vec<u32>> = spin::Once::new();

pub fn systick_irq() -> rdrive::IrqId {
    unsafe { TIMER_IRQ.expect("systick irq is not initialized") }
}

module_driver!(
    name: "ARMv8 Timer",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["arm,armv8-timer"],
            on_probe: probe
        }
    ],
);

pub(crate) fn setup_systick_irq() {
    let parent = unsafe { TIMER_IRQ_PARENT.expect("systick irq parent is not initialized") };
    let id = crate::irq::irq_setup_by_fdt(parent, TIMER_IRQ_VEC.wait());
    crate::irq::irq_set_enable(id, true);
}

fn probe(fdt: FdtInfo<'_>, dev: PlatformDevice) -> Result<(), OnProbeError> {
    let intc_id = dev.descriptor.irq_parent.unwrap();

    let mut intc = rdrive::get::<Intc>(intc_id).unwrap().lock().unwrap();
    let interrupts = fdt.interrupts();

    let irq = {
        #[cfg(not(feature = "hv"))]
        let irq_idx = 1;
        #[cfg(feature = "hv")]
        let irq_idx = 3;
        &interrupts[irq_idx].specifier
    };
    TIMER_IRQ_VEC.call_once(|| irq.to_vec());
    let irq = intc.setup_irq_by_fdt(irq);
    debug!("Armv8 timer irq: {:?}", irq);
    unsafe {
        TIMER_IRQ = Some(irq);
        TIMER_IRQ_PARENT = Some(intc_id);
    }
    Ok(())
}
