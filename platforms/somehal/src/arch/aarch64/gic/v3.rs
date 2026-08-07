use alloc::{collections::BTreeMap, format};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU64, Ordering},
};

use aarch64_cpu::{asm::barrier, registers::ID_AA64PFR0_EL1};
use arm_gic_driver::{checked_intid, v3::*};
use irq_framework::IrqId;
use kernutil::StaticCell;
use rdrive::{module_driver, probe::OnProbeError, register::ProbeFdt};

use crate::common::ioremap;

static CPU_IF_INIT: StaticCell<CpuInterfaceInit> = StaticCell::uninit();
static CPU_IF: StaticCell<BTreeMap<usize, CpuInterfaceSlot>> = StaticCell::uninit();
static PRIMARY_GICR_PHYS_BASE: AtomicU64 = AtomicU64::new(0);

struct CpuInterfaceSlot {
    inner: UnsafeCell<Option<CpuInterface>>,
}

// SAFETY: CPU_IF is initialized once by the BSP with all logical CPU slots
// preallocated, so the BTreeMap structure is immutable afterwards. Each CPU
// writes only its own slot during interrupt-controller initialization, and
// send_ipi reads the current CPU slot only after that CPU has initialized it.
unsafe impl Sync for CpuInterfaceSlot {}

impl CpuInterfaceSlot {
    const fn empty() -> Self {
        Self {
            inner: UnsafeCell::new(None),
        }
    }

    unsafe fn set(&self, cpu_idx: usize, cpu_if: CpuInterface) {
        let slot = unsafe { &mut *self.inner.get() };
        assert!(
            slot.is_none(),
            "GICv3 CPU interface for CPU index {cpu_idx} is already initialized"
        );
        *slot = Some(cpu_if);
    }

    unsafe fn get(&self, cpu_idx: usize) -> &CpuInterface {
        unsafe { &*self.inner.get() }.as_ref().unwrap_or_else(|| {
            panic!("GICv3 CPU interface for CPU index {cpu_idx} is not initialized")
        })
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
    init_cpu_interface_map();
    let cpu_idx =
        crate::cpu::current_cpu_idx().unwrap_or_else(someboot::smp::early_current_cpu_idx);
    init_cpu(cpu_idx);

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
        current_cpu_interface().set_irq_enable(intid, enable);
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

pub fn irq_set_trigger(irq: IrqId, trigger: Trigger) -> Result<(), crate::irq::IrqError> {
    super::trigger::dispatch_trigger_configuration(
        irq.hwirq.0,
        Some(super::its::LPI_INTID_BASE as u32),
        |raw| {
            let intid = checked_private_intid(raw)?;
            current_cpu_interface().set_cfg(intid, trigger);
            Ok(())
        },
        |raw| {
            super::with_gic_domain::<Gic, _>(irq.domain, |gic| {
                let intid = checked_runtime_intid(raw, gic.max_intid())?;
                gic.set_cfg(intid, trigger);
                Ok(())
            })?
        },
        || crate::irq::IrqError::Unsupported,
    )
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
        crate::irq::IrqAffinity::Fixed { cpu_id } => {
            Some(affinity_from_mpidr(super::hardware_cpu_id(cpu_id)?))
        }
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

pub fn send_ipi(raw: usize, target: crate::irq::IpiTarget) -> Result<(), crate::irq::IrqError> {
    let raw = u32::try_from(raw).map_err(|_| crate::irq::IrqError::InvalidIrq)?;
    if raw >= 16 {
        return Err(crate::irq::IrqError::InvalidIrq);
    }
    let sgi = IntId::sgi(raw);
    let target = match target {
        crate::irq::IpiTarget::Current => SGITarget::current(),
        crate::irq::IpiTarget::Cpu(cpu) => {
            SGITarget::list([affinity_from_mpidr(super::hardware_cpu_id(cpu.0)?)])
        }
    };
    // ICC_SGI1R_EL1 is the IPI doorbell. Complete prior Inner-Shareable
    // Normal-memory stores before issuing the SGI; the driver's trailing ISB
    // only forces execution of the system-register write.
    barrier::dsb(barrier::ISHST);
    current_cpu_interface().send_sgi(sgi, target);
    Ok(())
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

pub fn init_cpu(cpu_idx: usize) {
    if !CPU_IF_INIT.is_init() {
        warn!("failed to initialize GICv3 CPU interface for CPU {cpu_idx}: missing GICv3 state");
        return;
    }

    if let Err(err) = init_cpu_interface(cpu_idx) {
        warn!("failed to initialize GICv3 CPU interface for CPU {cpu_idx}: {err:?}");
    }

    debug!("GICCv3 initialized");
}

fn init_cpu_interface_map() {
    let mut cpu_if = BTreeMap::new();
    for cpu_idx in 0..someboot::smp::cpu_count() {
        cpu_if.insert(cpu_idx, CpuInterfaceSlot::empty());
    }
    CPU_IF.init(cpu_if);
}

fn init_cpu_interface(cpu_idx: usize) -> Result<(), &'static str> {
    let mut cpu = CPU_IF_INIT.cpu_interface();
    cpu.init_current_cpu()?;
    #[cfg(feature = "hv")]
    {
        // Hypervisor-owned physical interrupts must remain active after EOIR
        // until the guest completes the corresponding virtual interrupt. The
        // normal host path still performs both operations in `ActiveIrq::drop`.
        cpu.set_eoi_mode(true);
        info!("GICv3 CPU {cpu_idx} EOI mode: two_step={}", cpu.eoi_mode());
    }

    // SAFETY: CPU_IF was preallocated during BSP probe. Each CPU initializes
    // only its own logical CPU slot before it can send SGIs through that slot.
    unsafe { cpu_interface_slot(cpu_idx).set(cpu_idx, cpu) };
    Ok(())
}

fn current_cpu_interface() -> &'static CpuInterface {
    let cpu_idx = someboot::smp::early_current_cpu_idx();
    // SAFETY: GICv3 private IRQ operations can run before the OS per-CPU
    // register is initialized, so use the architecture CPU-id convention that
    // someboot also uses to enter this secondary CPU.
    unsafe { cpu_interface_slot(cpu_idx).get(cpu_idx) }
}

fn cpu_interface_slot(cpu_idx: usize) -> &'static CpuInterfaceSlot {
    CPU_IF
        .get(&cpu_idx)
        .unwrap_or_else(|| panic!("GICv3 CPU interface slot for CPU {cpu_idx} is not registered"))
}
