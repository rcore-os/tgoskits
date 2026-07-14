use alloc::{boxed::Box, format, vec::Vec};
use core::sync::atomic::{AtomicU8, Ordering};

use arm_gic_driver::{checked_intid, v2::*};
use irq_framework::IrqId;
use kernutil::StaticCell;
use rdrive::{module_driver, probe::OnProbeError, register::ProbeFdt};

use crate::common::ioremap;

static CPU_IF: StaticCell<CpuInterface> = StaticCell::uninit();
static TRAP: StaticCell<TrapOp> = StaticCell::uninit();
static IPI_TARGETS: StaticCell<Box<[AtomicU8]>> = StaticCell::uninit();
static CLAIMED_IPI_TARGETS: AtomicU8 = AtomicU8::new(0);

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
    init_ipi_targets()?;
    super::set_backend(super::GicBackend::V2);

    let cpu_idx = crate::cpu::current_cpu_idx().ok_or_else(|| {
        OnProbeError::other("current logical CPU is unavailable during GICv2 probe")
    })?;
    init_cpu(cpu_idx).map_err(OnProbeError::other)?;

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

pub fn init_cpu(cpu_idx: usize) -> Result<(), &'static str> {
    CPU_IF.init_current_cpu();
    #[cfg(feature = "hv")]
    CPU_IF.set_eoi_mode_ns(true);

    let target = CPU_IF
        .current_cpu_target()
        .ok_or("GICv2 did not expose one CPU-interface target bit")?;
    let target_bit = target.as_u8();
    let previous = CLAIMED_IPI_TARGETS.fetch_or(target_bit, Ordering::AcqRel);
    if previous & target_bit != 0 {
        return Err("GICv2 CPU-interface target bit is duplicated");
    }
    let slot = ipi_target_slot(cpu_idx).ok_or("GICv2 logical CPU target slot is missing")?;
    if slot
        .compare_exchange(0, target_bit, Ordering::Release, Ordering::Acquire)
        .is_err()
    {
        CLAIMED_IPI_TARGETS.fetch_and(!target_bit, Ordering::AcqRel);
        return Err("GICv2 logical CPU target was already published");
    }

    debug!("GICCv2 initialized");
    Ok(())
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
    let target_cpu =
        ipi_target(irq_framework::CpuId(cpu_id)).ok_or(crate::irq::IrqError::InvalidCpu)?;
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

pub fn send_ipi(
    raw: usize,
    target: crate::irq::CpuIpiTarget,
    current_cpu: irq_framework::CpuId,
) -> crate::irq::IpiSendStatus {
    let Ok(raw) = u32::try_from(raw) else {
        return crate::irq::IpiSendStatus::Invalid;
    };
    if raw >= 16 {
        return crate::irq::IpiSendStatus::Invalid;
    }
    let sgi = IntId::sgi(raw);
    let target = match target {
        crate::irq::CpuIpiTarget::Current { cpu } => {
            if current_cpu != cpu || ipi_target(cpu).is_none() {
                return crate::irq::IpiSendStatus::Invalid;
            }
            SGITarget::Current
        }
        crate::irq::CpuIpiTarget::Other { cpu } => {
            let Some(target_cpu) = ipi_target(cpu) else {
                return crate::irq::IpiSendStatus::Invalid;
            };
            SGITarget::TargetList(target_cpu)
        }
        crate::irq::CpuIpiTarget::AllExceptCurrent { current, cpu_count } => {
            if current_cpu != current
                || cpu_count != someboot::smp::runtime_cpu_count()
                || !(0..cpu_count).all(|cpu| ipi_target(irq_framework::CpuId(cpu)).is_some())
            {
                return crate::irq::IpiSendStatus::Invalid;
            }
            SGITarget::AllOther
        }
    };
    match CPU_IF.try_send_sgi(sgi, target) {
        Ok(()) => crate::irq::IpiSendStatus::Success,
        Err(_) => crate::irq::IpiSendStatus::Invalid,
    }
}

fn init_ipi_targets() -> Result<(), OnProbeError> {
    let cpu_count = someboot::smp::runtime_cpu_count();
    if cpu_count == 0 {
        return Err(OnProbeError::other("per-CPU metadata is not published"));
    }
    let targets = (0..cpu_count)
        .map(|_| AtomicU8::new(0))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    IPI_TARGETS.init(targets);
    Ok(())
}

fn ipi_target_slot(cpu_idx: usize) -> Option<&'static AtomicU8> {
    IPI_TARGETS
        .is_init()
        .then(|| IPI_TARGETS.get(cpu_idx))
        .flatten()
}

fn ipi_target(cpu: irq_framework::CpuId) -> Option<TargetList> {
    crate::cpu::runtime_cpu_target(cpu)?;
    let raw = ipi_target_slot(cpu.0)?.load(Ordering::Acquire);
    TargetList::from_one_hot(raw)
}
