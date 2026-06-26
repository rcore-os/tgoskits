use alloc::{collections::BTreeMap, format};
use core::cell::UnsafeCell;

use aarch64_cpu::registers::ID_AA64PFR0_EL1;
use arm_gic_driver::v3::*;
use irq_framework::IrqId;
use kernutil::StaticCell;
use rdrive::{module_driver, probe::OnProbeError, register::ProbeFdt};

use crate::common::ioremap;

static CPU_IF: StaticCell<BTreeMap<usize, CpuInterfaceSlot>> = StaticCell::uninit();

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

    init_cpu_interface_map();
    let cpu_idx =
        crate::cpu::current_cpu_idx().unwrap_or_else(someboot::smp::early_current_cpu_idx);
    init_cpu_interface(&gic, cpu_idx);

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
    super::with_gic_domain::<Gic, _>(irq.domain, |gic| {
        gic.set_irq_enable(unsafe { IntId::raw(irq.hwirq.0) }, enable);
    })
}

pub fn irq_set_affinity(
    irq: IrqId,
    affinity: crate::irq::IrqAffinity,
) -> Result<(), crate::irq::IrqError> {
    let intid = unsafe { IntId::raw(irq.hwirq.0) };
    if intid.is_private() {
        return Err(crate::irq::IrqError::Unsupported);
    }
    let target = match affinity {
        crate::irq::IrqAffinity::Any => None,
        crate::irq::IrqAffinity::Fixed { cpu_id } => {
            Some(affinity_from_mpidr(super::hardware_cpu_id(cpu_id)))
        }
    };
    super::with_gic_domain::<Gic, _>(irq.domain, |gic| gic.set_target_cpu(intid, target))?;
    Ok(())
}

pub fn send_ipi(raw: usize, target: crate::irq::IpiTarget) {
    let sgi = IntId::sgi(raw as u32);
    let target = match target {
        crate::irq::IpiTarget::Current { cpu_id: _ } => SGITarget::current(),
        crate::irq::IpiTarget::Other { cpu_id: cpu_idx } => {
            SGITarget::list([affinity_from_mpidr(super::hardware_cpu_id(cpu_idx))])
        }
        crate::irq::IpiTarget::AllExceptCurrent { .. } => SGITarget::All,
    };
    current_cpu_interface().send_sgi(sgi, target);
}

fn affinity_from_mpidr(mpidr: usize) -> Affinity {
    Affinity::from_mpidr(mpidr as u64)
}

pub fn init_cpu(cpu_idx: usize) {
    if let Err(err) = super::with_primary_gic::<Gic, _>(|gic| init_cpu_interface(gic, cpu_idx)) {
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

fn init_cpu_interface(gic: &Gic, cpu_idx: usize) {
    let mut cpu = gic.cpu_interface();
    cpu.init_current_cpu().unwrap();
    #[cfg(feature = "hv")]
    cpu.set_eoi_mode(true);

    // SAFETY: CPU_IF was preallocated during BSP probe. Each CPU initializes
    // only its own logical CPU slot before it can send SGIs through that slot.
    unsafe { cpu_interface_slot(cpu_idx).set(cpu_idx, cpu) };
}

fn current_cpu_interface() -> &'static CpuInterface {
    let cpu_idx = crate::cpu::current_cpu_idx()
        .unwrap_or_else(|| panic!("current logical CPU index is not available for GICv3 SGI"));
    // SAFETY: send_ipi is only valid after the current CPU has completed
    // interrupt-controller initialization and stored its CpuInterface.
    unsafe { cpu_interface_slot(cpu_idx).get(cpu_idx) }
}

fn cpu_interface_slot(cpu_idx: usize) -> &'static CpuInterfaceSlot {
    CPU_IF
        .get(&cpu_idx)
        .unwrap_or_else(|| panic!("GICv3 CPU interface slot for CPU {cpu_idx} is not registered"))
}
