use alloc::format;

use aarch64_cpu::registers::ID_AA64PFR0_EL1;
use arm_gic_driver::v3::*;
use kernutil::StaticCell;
use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo};

use crate::common::ioremap;

static CPU_IF: StaticCell<CpuInterface> = StaticCell::uninit();

pub fn with_gic(f: impl FnOnce(&mut Gic)) {
    let mut gic = super::get_gicd().lock().unwrap();
    if let Some(gic) = gic.typed_mut::<Gic>() {
        f(gic);
    }
}

module_driver!(
    name: "GICv3",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["arm,gic-v3"],
            on_probe: probe_gic
        }
    ],
);

fn probe_gic(info: FdtInfo<'_>, dev: PlatformDevice) -> Result<(), OnProbeError> {
    let mut reg = info.node.regs().into_iter();
    let gicd_reg = reg.next().ok_or(OnProbeError::other(format!(
        "[{}] has no reg",
        info.node.name()
    )))?;
    let gicr_reg = reg.next().unwrap();

    let gicd = ioremap(
        gicd_reg.address,
        gicd_reg.size.unwrap_or(0x1000).try_into().unwrap(),
    )
    .unwrap();
    let gicr = ioremap(
        gicr_reg.address,
        gicr_reg.size.unwrap_or(0x1000).try_into().unwrap(),
    )
    .unwrap();

    let mut gic = unsafe { Gic::new(gicd.as_ptr().into(), gicr.as_ptr().into()) };
    gic.init();
    let cpu = gic.cpu_interface();
    CPU_IF.init(cpu);

    init_cpu();

    dev.register(rdif_intc::Intc::new(gic));

    Ok(())
}

/// Check if support GIC cpu interface.
pub fn is_support_icc() -> bool {
    let val = ID_AA64PFR0_EL1.get();
    // Check GIC field
    val >> 24 & 0xf > 0
}

pub fn handle_irq() -> someboot::irq::IrqId {
    let ack = ack1();

    super::_handle_irq(someboot::irq::IrqId::new(ack.to_u32() as _));

    if !ack.is_special() {
        eoi1(ack);
        if eoi_mode() {
            dir(ack);
        }
    }
    let id: u32 = ack.into();
    (id as usize).into()
}

pub fn irq_set_enable(raw: usize, enable: bool) {
    with_gic(|gic| {
        gic.set_irq_enable(unsafe { IntId::raw(raw as _) }, enable);
    });
}

pub fn init_cpu() {
    unsafe {
        CPU_IF.update(|cpu| {
            cpu.init_current_cpu().unwrap();
            #[cfg(feature = "hv")]
            cpu.set_eoi_mode(true);
        });
    }

    debug!("GICCv3 initialized");
}
