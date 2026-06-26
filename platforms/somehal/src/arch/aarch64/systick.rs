use alloc::vec::Vec;

use rdif_intc::Intc;
use rdrive::{module_driver, probe::OnProbeError, register::ProbeFdt};

static mut TIMER_IRQ: Option<irq_framework::IrqId> = None;
static mut TIMER_IRQ_PARENT: Option<rdrive::DeviceId> = None;
static TIMER_IRQ_VEC: spin::Once<Vec<u32>> = spin::Once::new();

pub fn systick_irq() -> irq_framework::IrqId {
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
    let Ok(id) = crate::irq::irq_setup_by_fdt(parent, TIMER_IRQ_VEC.wait()) else {
        warn!("failed to setup ARMv8 timer IRQ from FDT");
        return;
    };
    if let Err(err) = crate::irq::irq_set_enable(id, true) {
        warn!("failed to enable ARMv8 timer IRQ {id:?}: {err:?}");
    }
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (fdt, dev) = probe.into_parts();
    let intc_id = dev.descriptor.irq_parent.unwrap();

    let mut intc = rdrive::get::<Intc>(intc_id).unwrap().lock().unwrap();
    let interrupts = fdt.interrupts();

    let irq_idx = someboot::timer::aarch64_timer_irq_index(someboot::timer::aarch64_timer_mode());
    let irq = &interrupts[irq_idx].specifier;
    TIMER_IRQ_VEC.call_once(|| irq.to_vec());
    let translation = intc
        .translate_fdt(irq)
        .map_err(|err| OnProbeError::other(alloc::format!("invalid timer IRQ: {err:?}")))?;
    intc.configure(&translation).map_err(|err| {
        OnProbeError::other(alloc::format!("failed to configure timer IRQ: {err:?}"))
    })?;
    let irq = translation.id;
    debug!("Armv8 timer irq: {:?}", irq);
    unsafe {
        TIMER_IRQ = Some(irq);
        TIMER_IRQ_PARENT = Some(intc_id);
    }
    Ok(())
}
