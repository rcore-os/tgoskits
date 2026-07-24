use alloc::format;
use core::sync::atomic::{AtomicU8, Ordering};

use arm_gic_driver::{checked_intid, v2::*};
use irq_framework::IrqId;
use kernutil::StaticCell;
use rdrive::{module_driver, probe::OnProbeError, register::ProbeFdt};

use crate::common::ioremap;

static CPU_IF: StaticCell<CpuInterface> = StaticCell::uninit();
static TRAP: StaticCell<TrapOp> = StaticCell::uninit();
const MAX_GIC_V2_CPU_INTERFACES: usize = 8;
static CPU_TARGETS: [AtomicU8; MAX_GIC_V2_CPU_INTERFACES] =
    [const { AtomicU8::new(0) }; MAX_GIC_V2_CPU_INTERFACES];

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

    let cpu_idx = crate::cpu::current_cpu_idx()
        .unwrap_or_else(|| panic!("current logical CPU index is not available for GICv2 init"));
    init_cpu(cpu_idx);

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

pub fn init_cpu(cpu_idx: usize) {
    let target = unsafe {
        CPU_IF.update(|cpu| {
            cpu.init_current_cpu();
            #[cfg(feature = "hv")]
            cpu.set_eoi_mode_ns(true);
            cpu.current_cpu_target()
        })
    };
    record_cpu_target(cpu_idx, target);

    debug!(
        "GICCv2 initialized for logical CPU {cpu_idx}, target mask {:#04x}",
        target.as_u8()
    );
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
    let target_cpu = cpu_target(cpu_id).ok_or(crate::irq::IrqError::InvalidIrq)?;
    super::with_gic_domain::<Gic, _>(irq.domain, |gic| {
        let intid = checked_runtime_intid(irq.hwirq.0, gic.max_intid())?;
        gic.set_target_cpu(intid, target_cpu);
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
            let target_cpu = cpu_target(cpu_id).unwrap_or_else(|| {
                panic!("GICv2 target is not initialized for logical CPU {cpu_id}")
            });
            SGITarget::TargetList(target_cpu)
        }
        crate::irq::IpiTarget::AllExceptCurrent { .. } => SGITarget::AllOther,
    };
    CPU_IF.send_sgi(sgi, target);
}

fn record_cpu_target(cpu_idx: usize, target: TargetList) {
    let target_mask = target.as_u8();
    assert!(
        target_mask.is_power_of_two(),
        "Invalid GICv2 target mask {target_mask:#04x} for logical CPU {cpu_idx}"
    );
    let slot = CPU_TARGETS
        .get(cpu_idx)
        .unwrap_or_else(|| panic!("GICv2 logical CPU index out of range: {cpu_idx}"));
    slot.store(target_mask, Ordering::Release);
}

fn cpu_target(cpu_idx: usize) -> Option<TargetList> {
    let target_mask = CPU_TARGETS.get(cpu_idx)?.load(Ordering::Acquire);
    (target_mask != 0).then(|| TargetList::from_raw(target_mask))
}
