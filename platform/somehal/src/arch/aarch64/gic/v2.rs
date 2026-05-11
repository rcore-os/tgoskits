use alloc::format;

use arm_gic_driver::v2::*;
use kernutil::StaticCell;
use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo};

use crate::common::ioremap;

static CPU_IF: StaticCell<CpuInterface> = StaticCell::uninit();
static TRAP: StaticCell<TrapOp> = StaticCell::uninit();

pub fn with_gic(f: impl FnOnce(&mut Gic)) {
    let mut gic = super::get_gicd().lock().unwrap();
    if let Some(gic) = gic.typed_mut::<Gic>() {
        f(gic);
    }
}

module_driver!(
    name: "GICv2",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["arm,cortex-a15-gic", "arm,gic-400"],
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

    let mut hyper = None;

    if let Some(gich_reg) = reg.next()
        && let Some(gicv_reg) = reg.next()
    {
        let gich = ioremap(
            gich_reg.address,
            gich_reg.size.unwrap_or(0x1000).try_into().unwrap(),
        )
        .unwrap();
        let gicv = ioremap(
            gicv_reg.address,
            gicv_reg.size.unwrap_or(0x1000).try_into().unwrap(),
        )
        .unwrap();

        hyper = Some(HyperAddress::new(
            gich.as_ptr().into(),
            gicv.as_ptr().into(),
        ))
    }

    let mut gic = unsafe { Gic::new(gicd.as_ptr().into(), gicr.as_ptr().into(), hyper) };
    gic.init();
    let cpu = gic.cpu_interface();
    let trap = cpu.trap_operations();
    CPU_IF.init(cpu);
    TRAP.init(trap);

    init_cpu();

    dev.register(rdif_intc::Intc::new(gic));

    Ok(())
}

pub fn handle_irq() -> someboot::irq::IrqId {
    let ack = TRAP.ack();

    let irq_num = match ack {
        Ack::Other(intid) => intid,
        Ack::SGI { intid, cpu_id: _ } => intid,
    };

    let irq_num: u32 = irq_num.into();
    super::_handle_irq(someboot::irq::IrqId::new(irq_num as _));

    if !ack.is_special() {
        TRAP.eoi(ack);
        if TRAP.eoi_mode_ns() {
            TRAP.dir(ack);
        }
    }
    irq_num.into()
}

pub fn init_cpu() {
    unsafe {
        CPU_IF.update(|cpu| {
            cpu.init_current_cpu();
            #[cfg(feature = "hv")]
            cpu.set_eoi_mode_ns(true);
        });
    }

    debug!("GICCv2 initialized");
}

pub fn irq_set_enable(raw: usize, enable: bool) {
    with_gic(|gic| {
        gic.set_irq_enable(unsafe { IntId::raw(raw as _) }, enable);
    });
}
