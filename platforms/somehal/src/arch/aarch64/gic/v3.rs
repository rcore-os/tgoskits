use alloc::{boxed::Box, format, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use aarch64_cpu::registers::ID_AA64PFR0_EL1;
use arm_gic_driver::{checked_intid, v3::*};
use irq_framework::IrqId;
use kernutil::StaticCell;
use rdrive::{module_driver, probe::OnProbeError, register::ProbeFdt};

use crate::common::ioremap;

static CPU_IF_INIT: StaticCell<CpuInterfaceInit> = StaticCell::uninit();
static CPU_IF: StaticCell<Box<[CpuInterfaceSlot]>> = StaticCell::uninit();
static PRIMARY_GICR_PHYS_BASE: AtomicU64 = AtomicU64::new(0);

struct CpuInterfaceSlot {
    claimed: AtomicBool,
    inner: spin::Once<CpuInterface>,
}

impl CpuInterfaceSlot {
    const fn empty() -> Self {
        Self {
            claimed: AtomicBool::new(false),
            inner: spin::Once::new(),
        }
    }

    fn set(&self, cpu_if: CpuInterface) -> Result<(), &'static str> {
        if self
            .claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("GICv3 CPU interface slot is already initialized");
        }
        self.inner.call_once(|| cpu_if);
        Ok(())
    }

    fn get(&self) -> Option<&CpuInterface> {
        self.inner.get()
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

fn probe_gic(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let mut reg = info.node.regs().into_iter();
    let gicd_reg = reg.next().ok_or(OnProbeError::other(format!(
        "[{}] has no reg",
        info.node.name()
    )))?;
    let gicr_reg = reg.next().unwrap();
    PRIMARY_GICR_PHYS_BASE.store(gicr_reg.address, Ordering::Release);

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
    super::set_backend(super::GicBackend::V3);

    CPU_IF_INIT.init(gic.cpu_interface_init());
    init_cpu_interface_map()?;
    validate_ipi_topology()?;
    let cpu_idx =
        crate::cpu::current_cpu_idx().unwrap_or_else(someboot::smp::early_current_cpu_idx);
    init_cpu(cpu_idx).map_err(OnProbeError::other)?;

    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::AArch64Gic,
    )
    .map_err(|err| OnProbeError::other(format!("failed to register GICv3 domain: {err:?}")))?;
    dev.register(rdif_intc::Intc::new(domain, gic));

    Ok(())
}

/// Check if support GIC cpu interface.
pub fn is_support_icc() -> bool {
    let val = ID_AA64PFR0_EL1.get();
    // Check GIC field
    val >> 24 & 0xf > 0
}

pub struct ActiveIrq {
    irq: rdrive::IrqId,
    ack: IntId,
}

impl ActiveIrq {
    pub fn id(&self) -> rdrive::IrqId {
        self.irq
    }
}

impl Drop for ActiveIrq {
    fn drop(&mut self) {
        eoi1(self.ack);
        if eoi_mode() {
            dir(self.ack);
        }
    }
}

pub fn begin_irq() -> Option<ActiveIrq> {
    let ack = ack1();
    if ack.is_special() {
        return None;
    }

    Some(ActiveIrq {
        irq: (ack.to_u32() as usize).into(),
        ack,
    })
}

pub fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), crate::irq::IrqError> {
    if irq.hwirq.0 < 32 {
        let intid = checked_private_intid(irq.hwirq.0)?;
        current_cpu_interface()?.set_irq_enable(intid, enable);
        return Ok(());
    }
    if irq.hwirq.0 >= super::its::LPI_INTID_BASE {
        return super::its::set_lpi_enabled(irq, enable);
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
    if irq.hwirq.0 >= super::its::LPI_INTID_BASE {
        return super::its::set_lpi_affinity(irq, affinity);
    }
    let target = match affinity {
        crate::irq::IrqAffinity::Any => None,
        crate::irq::IrqAffinity::Fixed { cpu_id } => Some(
            ipi_affinity(irq_framework::CpuId(cpu_id)).ok_or(crate::irq::IrqError::InvalidCpu)?,
        ),
    };
    super::with_gic_domain::<Gic, _>(irq.domain, |gic| {
        let intid = checked_runtime_intid(irq.hwirq.0, gic.max_intid())?;
        gic.set_target_cpu(intid, target);
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
            if current_cpu != cpu {
                return crate::irq::IpiSendStatus::Invalid;
            }
            let Some(affinity) = ready_ipi_affinity(cpu) else {
                return crate::irq::IpiSendStatus::Invalid;
            };
            let Ok(target) = SGITarget::try_list([affinity]) else {
                return crate::irq::IpiSendStatus::Invalid;
            };
            target
        }
        crate::irq::CpuIpiTarget::Other { cpu } => {
            let Some(affinity) = ready_ipi_affinity(cpu) else {
                return crate::irq::IpiSendStatus::Invalid;
            };
            let Ok(target) = SGITarget::try_list([affinity]) else {
                return crate::irq::IpiSendStatus::Invalid;
            };
            target
        }
        crate::irq::CpuIpiTarget::AllExceptCurrent { current, cpu_count } => {
            if current_cpu != current
                || cpu_count != someboot::smp::runtime_cpu_count()
                || !(0..cpu_count).all(|cpu| cpu_interface_ready(irq_framework::CpuId(cpu)))
            {
                return crate::irq::IpiSendStatus::Invalid;
            }
            SGITarget::All
        }
    };
    match try_send_sgi(sgi, target) {
        Ok(()) => crate::irq::IpiSendStatus::Success,
        Err(_) => crate::irq::IpiSendStatus::Invalid,
    }
}

fn affinity_from_mpidr(mpidr: usize) -> Affinity {
    Affinity::from_mpidr(mpidr as u64)
}

pub(super) fn primary_gicr_phys_base() -> Option<u64> {
    match PRIMARY_GICR_PHYS_BASE.load(Ordering::Acquire) {
        0 => None,
        phys => Some(phys),
    }
}

pub fn init_cpu(cpu_idx: usize) -> Result<(), &'static str> {
    if !CPU_IF_INIT.is_init() {
        return Err("missing GICv3 CPU-interface initialization state");
    }

    init_cpu_interface(cpu_idx)?;

    debug!("GICCv3 initialized");
    Ok(())
}

fn init_cpu_interface_map() -> Result<(), OnProbeError> {
    let cpu_count = someboot::smp::runtime_cpu_count();
    if cpu_count == 0 {
        return Err(OnProbeError::other("per-CPU metadata is not published"));
    }
    let cpu_if = (0..cpu_count)
        .map(|_| CpuInterfaceSlot::empty())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    CPU_IF.init(cpu_if);
    Ok(())
}

fn init_cpu_interface(cpu_idx: usize) -> Result<(), &'static str> {
    let mut cpu = CPU_IF_INIT
        .try_cpu_interface()
        .ok_or("GICv3 redistributor for the current CPU is missing")?;
    cpu.init_current_cpu()?;
    #[cfg(feature = "hv")]
    cpu.set_eoi_mode(true);

    let slot = cpu_interface_slot(cpu_idx).ok_or("GICv3 logical CPU slot is missing")?;
    slot.set(cpu)
}

fn current_cpu_interface() -> Result<&'static CpuInterface, crate::irq::IrqError> {
    let cpu = crate::cpu::runtime_current_cpu().ok_or(crate::irq::IrqError::InvalidCpu)?;
    let slot = cpu_interface_slot(cpu.0).ok_or(crate::irq::IrqError::InvalidCpu)?;
    slot.get().ok_or(crate::irq::IrqError::InvalidCpu)
}

fn cpu_interface_slot(cpu_idx: usize) -> Option<&'static CpuInterfaceSlot> {
    CPU_IF.is_init().then(|| CPU_IF.get(cpu_idx)).flatten()
}

fn ipi_affinity(cpu: irq_framework::CpuId) -> Option<Affinity> {
    let target = crate::cpu::runtime_cpu_target(cpu)?;
    let affinity = affinity_from_mpidr(target.as_usize());
    (affinity.aff0 < 16).then_some(affinity)
}

fn ready_ipi_affinity(cpu: irq_framework::CpuId) -> Option<Affinity> {
    cpu_interface_ready(cpu)
        .then(|| ipi_affinity(cpu))
        .flatten()
}

fn cpu_interface_ready(cpu: irq_framework::CpuId) -> bool {
    crate::cpu::runtime_cpu_target(cpu).is_some()
        && cpu_interface_slot(cpu.0)
            .and_then(CpuInterfaceSlot::get)
            .is_some()
}

fn validate_ipi_topology() -> Result<(), OnProbeError> {
    let mut seen = Vec::new();
    for cpu_idx in 0..someboot::smp::runtime_cpu_count() {
        let affinity = ipi_affinity(irq_framework::CpuId(cpu_idx)).ok_or_else(|| {
            OnProbeError::other(format!(
                "CPU {cpu_idx} requires unsupported GICv3 SGI range selection"
            ))
        })?;
        let encoded = u32::from(affinity.aff0)
            | (u32::from(affinity.aff1) << 8)
            | (u32::from(affinity.aff2) << 16)
            | (u32::from(affinity.aff3) << 24);
        if seen.contains(&encoded) {
            return Err(OnProbeError::other(format!(
                "duplicate GICv3 affinity for logical CPU {cpu_idx}"
            )));
        }
        seen.push(encoded);
    }
    Ok(())
}
