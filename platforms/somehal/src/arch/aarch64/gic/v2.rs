use alloc::format;

use arm_gic_driver::{checked_intid, v2::*};
use irq_framework::IrqId;
use kernutil::StaticCell;
use rdrive::{module_driver, probe::OnProbeError, register::ProbeFdt};

use crate::common::ioremap;

static CPU_IF: StaticCell<CpuInterface> = StaticCell::uninit();
static TRAP: StaticCell<TrapOp> = StaticCell::uninit();

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

fn probe_gic(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
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
    super::set_backend(super::GicBackend::V2);

    init_cpu();

    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::AArch64Gic,
    )
    .map_err(|err| OnProbeError::other(format!("failed to register GICv2 domain: {err:?}")))?;
    dev.register(rdif_intc::Intc::new(domain, gic));

    Ok(())
}

pub struct ActiveIrq {
    irq: rdrive::IrqId,
    ack: Ack,
}

impl ActiveIrq {
    pub fn id(&self) -> rdrive::IrqId {
        self.irq
    }
}

impl Drop for ActiveIrq {
    fn drop(&mut self) {
        TRAP.eoi(self.ack);
        if TRAP.eoi_mode_ns() {
            TRAP.dir(self.ack);
        }
    }
}

pub fn begin_irq() -> Option<ActiveIrq> {
    let ack = TRAP.ack();
    if ack.is_special() {
        return None;
    }

    let irq_num = match ack {
        Ack::Other(intid) => intid,
        Ack::SGI { intid, cpu_id: _ } => intid,
    };

    let irq_num: u32 = irq_num.into();
    Some(ActiveIrq {
        irq: (irq_num as usize).into(),
        ack,
    })
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

pub fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), crate::irq::IrqError> {
    if irq.hwirq.0 < 32 {
        let intid = checked_private_intid(irq.hwirq.0)?;
        CPU_IF.set_irq_enable(intid, enable);
        return Ok(());
    }

    super::with_gic_domain::<Gic, _>(irq.domain, |gic| {
        let intid = checked_runtime_intid(irq.hwirq.0, gic.max_intid())?;
        gic.set_irq_enable(intid, enable);
        Ok(())
    })?
}

pub fn irq_set_affinity(
    irq: IrqId,
    affinity: crate::irq::IrqAffinity,
) -> Result<(), crate::irq::IrqError> {
    if irq.hwirq.0 < 32 {
        return Err(crate::irq::IrqError::Unsupported);
    }
    let crate::irq::IrqAffinity::Fixed { cpu_id } = affinity else {
        return Ok(());
    };
    let target_cpu = super::hardware_cpu_id(cpu_id);
    super::with_gic_domain::<Gic, _>(irq.domain, |gic| {
        let intid = checked_runtime_intid(irq.hwirq.0, gic.max_intid())?;
        gic.set_target_cpu(intid, TargetList::new(&mut core::iter::once(target_cpu)));
        Ok::<(), crate::irq::IrqError>(())
    })??;
    Ok(())
}

fn checked_private_intid(raw: u32) -> Result<IntId, crate::irq::IrqError> {
    checked_runtime_intid(raw, 32)
}

fn checked_runtime_intid(raw: u32, max_intid: u32) -> Result<IntId, crate::irq::IrqError> {
    checked_intid(raw, max_intid).map_err(|_| crate::irq::IrqError::InvalidIrq)
}

pub fn send_ipi(raw: usize, target: crate::irq::IpiTarget) {
    let sgi = IntId::sgi(raw as u32);
    let target = match target {
        crate::irq::IpiTarget::Current { cpu_id: _ } => SGITarget::Current,
        crate::irq::IpiTarget::Other { cpu_id } => {
            let target_cpu = super::hardware_cpu_id(cpu_id);
            SGITarget::TargetList(TargetList::new(&mut core::iter::once(target_cpu)))
        }
        crate::irq::IpiTarget::AllExceptCurrent { .. } => SGITarget::AllOther,
    };
    CPU_IF.send_sgi(sgi, target);
}
