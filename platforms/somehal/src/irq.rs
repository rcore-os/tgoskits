use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};

use ax_kspin::SpinRaw as Mutex;
pub use rdif_intc;
use rdif_intc::Intc;
pub type ControllerIrqId = irq_framework::IrqId;
pub use irq_framework::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, HwIrq, IrqDomainId, IrqError,
    IrqId, IrqSource,
};
use rdrive::{Device, DeviceId};

use crate::{arch::Plat, common::PlatOp};

/// CPU-local interrupt domain for architecture trap causes such as timers/IPIs.
pub const CPU_LOCAL_IRQ_DOMAIN: IrqDomainId = IrqDomainId(u16::MAX);

/// x86 local APIC interrupt domain.
pub const X86_LAPIC_DOMAIN: IrqDomainId = IrqDomainId(1);

/// x86 I/O APIC interrupt domain.
pub const X86_IOAPIC_DOMAIN: IrqDomainId = IrqDomainId(2);

/// AArch64 GIC interrupt domain.
pub const AARCH64_GIC_DOMAIN: IrqDomainId = IrqDomainId(3);

/// RISC-V PLIC interrupt domain.
pub const RISCV_PLIC_DOMAIN: IrqDomainId = IrqDomainId(4);

/// LoongArch EIOINTC interrupt domain.
pub const LOONGARCH_EIOINTC_DOMAIN: IrqDomainId = IrqDomainId(5);

/// LoongArch PCH-PIC interrupt domain.
pub const LOONGARCH_PCH_PIC_DOMAIN: IrqDomainId = IrqDomainId(6);

const DYNAMIC_IRQ_DOMAIN_BASE: u16 = 7;
const INVALID_IRQ_DOMAIN: u16 = u16::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqDomainKind {
    X86IoApic,
    AArch64Gic,
    RiscvPlic,
    LoongArchEioIntc,
    LoongArchPchPic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqDomain {
    pub id: IrqDomainId,
    pub owner: DeviceId,
    pub kind: IrqDomainKind,
}

static IRQ_DOMAINS: Mutex<Vec<IrqDomain>> = Mutex::new(Vec::new());
static X86_IOAPIC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);
static AARCH64_GIC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);
static RISCV_PLIC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);
static LOONGARCH_EIOINTC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);
static LOONGARCH_PCH_PIC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);

pub fn alloc_irq_domain(owner: DeviceId, kind: IrqDomainKind) -> Result<IrqDomainId, IrqError> {
    register_irq_domain(owner, None, kind)
}

pub fn register_irq_domain(
    owner: DeviceId,
    preferred: Option<IrqDomainId>,
    kind: IrqDomainKind,
) -> Result<IrqDomainId, IrqError> {
    let mut domains = IRQ_DOMAINS.lock();
    if let Some(domain) = domains.iter().find(|domain| domain.owner == owner) {
        return if domain.kind == kind {
            Ok(domain.id)
        } else {
            Err(IrqError::Busy)
        };
    }

    if domains.iter().any(|domain| domain.kind == kind) {
        return Err(IrqError::Unsupported);
    }

    let id = match preferred {
        Some(id) => {
            if is_reserved_domain(id) {
                return Err(IrqError::InvalidIrq);
            }
            if domains.iter().any(|domain| domain.id == id) {
                return Err(IrqError::Busy);
            }
            id
        }
        None => next_dynamic_domain(&domains)?,
    };

    domains.push(IrqDomain { id, owner, kind });
    domain_slot(kind).store(id.0, Ordering::Release);
    Ok(id)
}

fn domain_slot(kind: IrqDomainKind) -> &'static AtomicU16 {
    match kind {
        IrqDomainKind::X86IoApic => &X86_IOAPIC_DOMAIN_SLOT,
        IrqDomainKind::AArch64Gic => &AARCH64_GIC_DOMAIN_SLOT,
        IrqDomainKind::RiscvPlic => &RISCV_PLIC_DOMAIN_SLOT,
        IrqDomainKind::LoongArchEioIntc => &LOONGARCH_EIOINTC_DOMAIN_SLOT,
        IrqDomainKind::LoongArchPchPic => &LOONGARCH_PCH_PIC_DOMAIN_SLOT,
    }
}

fn is_reserved_domain(id: IrqDomainId) -> bool {
    id.0 < DYNAMIC_IRQ_DOMAIN_BASE || id.0 == u16::MAX
}

fn next_dynamic_domain(domains: &[IrqDomain]) -> Result<IrqDomainId, IrqError> {
    for id in DYNAMIC_IRQ_DOMAIN_BASE..u16::MAX {
        let id = IrqDomainId(id);
        if domains.iter().all(|domain| domain.id != id) {
            return Ok(id);
        }
    }
    Err(IrqError::NoMemory)
}

pub fn domain_by_id(id: IrqDomainId) -> Option<IrqDomain> {
    IRQ_DOMAINS
        .lock()
        .iter()
        .find(|domain| domain.id == id)
        .copied()
}

pub fn domain_by_owner(owner: DeviceId) -> Option<IrqDomain> {
    IRQ_DOMAINS
        .lock()
        .iter()
        .find(|domain| domain.owner == owner)
        .copied()
}

pub fn domain_by_kind(kind: IrqDomainKind) -> Option<IrqDomain> {
    domain_by_kind_fast(kind).and_then(domain_by_id)
}

pub fn domain_by_kind_fast(kind: IrqDomainKind) -> Option<IrqDomainId> {
    match domain_slot(kind).load(Ordering::Acquire) {
        INVALID_IRQ_DOMAIN => None,
        id => Some(IrqDomainId(id)),
    }
}

pub fn domain_is_kind(id: IrqDomainId, kind: IrqDomainKind) -> bool {
    domain_by_kind_fast(kind) == Some(id)
}

pub fn intc_by_domain(domain: IrqDomainId) -> Result<Device<Intc>, IrqError> {
    let domain = domain_by_id(domain).ok_or(IrqError::Unsupported)?;
    rdrive::get::<Intc>(domain.owner).map_err(|_| IrqError::Unsupported)
}

pub fn set_controller_irq_enabled(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
    #[cfg(target_arch = "x86_64")]
    if domain_is_kind(irq.domain, IrqDomainKind::X86IoApic) {
        return crate::arch::set_ioapic_gsi_enabled_from_irq(irq.hwirq.0, enabled);
    }

    let intc = intc_by_domain(irq.domain)?;
    let mut intc = intc.try_lock().map_err(|_| IrqError::Busy)?;
    intc.set_enabled(irq.hwirq, enabled)
}

#[must_use = "dropping ActiveIrq completes the interrupt in the interrupt controller"]
pub struct ActiveIrq {
    inner: <Plat as PlatOp>::ActiveIrq,
}

impl ActiveIrq {
    pub fn id(&self) -> IrqId {
        Plat::active_irq_id(&self.inner)
    }
}

/// Target specification for inter-processor interrupts.
#[derive(Clone, Copy, Debug)]
pub enum IpiTarget {
    /// Send to the current CPU.
    Current {
        /// The logical CPU ID of the current CPU.
        cpu_id: usize,
    },
    /// Send to a specific CPU.
    Other {
        /// The logical CPU ID of the target CPU.
        cpu_id: usize,
    },
    /// Send to all other CPUs.
    AllExceptCurrent {
        /// The logical CPU ID of the current CPU.
        cpu_id: usize,
        /// The total number of CPUs.
        cpu_num: usize,
    },
}

/// Hardware routing preference for a global IRQ line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqAffinity {
    /// Leave routing unchanged or platform-selected.
    Any,
    /// Route to one logical CPU.
    Fixed { cpu_id: usize },
}

fn setup_irq_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> Result<IrqId, IrqError> {
    let mut intc = rdrive::get::<Intc>(irq_parent)
        .map_err(|_| IrqError::Unsupported)?
        .lock()
        .map_err(|_| IrqError::Controller)?;
    debug!("Setting up IRQ {:?}", irq_cell);
    let translation = intc.translate_fdt(irq_cell)?;
    intc.configure(&translation)?;
    Ok(translation.id)
}

#[cfg(target_arch = "aarch64")]
pub fn irq_setup_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> Result<IrqId, IrqError> {
    setup_irq_by_fdt(irq_parent, irq_cell)
}

#[cfg(target_arch = "riscv64")]
pub fn irq_setup_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> Result<IrqId, IrqError> {
    setup_irq_by_fdt(irq_parent, irq_cell)
}

#[cfg(target_arch = "loongarch64")]
pub fn irq_setup_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> Result<IrqId, IrqError> {
    setup_irq_by_fdt(irq_parent, irq_cell)
}

#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "loongarch64"
)))]
pub fn irq_setup_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> Result<IrqId, IrqError> {
    setup_irq_by_fdt(irq_parent, irq_cell)
}

pub fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), IrqError> {
    debug!("Setting IRQ {:?} enable to {}", irq, enable);
    Plat::irq_set_enable(irq, enable)
}

pub fn irq_set_affinity(irq: IrqId, affinity: IrqAffinity) -> Result<(), IrqError> {
    Plat::irq_set_affinity(irq, affinity)
}

pub fn send_ipi(irq: IrqId, target: IpiTarget) {
    Plat::send_ipi(irq, target);
}

pub fn ipi_irq() -> IrqId {
    Plat::ipi_irq()
}

pub fn systick_irq() -> IrqId {
    Plat::systick_irq()
}

#[cfg(target_arch = "aarch64")]
pub fn aarch64_gic_irq_id(hwirq: HwIrq) -> IrqId {
    crate::arch::gic_irq_id(hwirq)
}

#[cfg(target_arch = "aarch64")]
pub fn aarch64_gic_irq_id_checked(hwirq: HwIrq) -> Result<IrqId, IrqError> {
    crate::arch::gic_irq_id_checked(hwirq)
}

pub fn begin_irq(raw: usize) -> Option<ActiveIrq> {
    Plat::begin_irq(raw).map(|inner| ActiveIrq { inner })
}

pub fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError> {
    Plat::resolve_irq_source(source)
}

pub fn send_ipi_to_cpu(cpu_id: usize) {
    Plat::send_ipi_to_cpu(cpu_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset_domains() {
        IRQ_DOMAINS.lock().clear();
        for kind in [
            IrqDomainKind::X86IoApic,
            IrqDomainKind::AArch64Gic,
            IrqDomainKind::RiscvPlic,
            IrqDomainKind::LoongArchEioIntc,
            IrqDomainKind::LoongArchPchPic,
        ] {
            domain_slot(kind).store(INVALID_IRQ_DOMAIN, Ordering::Release);
        }
    }

    #[test]
    fn alloc_irq_domain_starts_after_compat_domains_and_is_idempotent() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        let owner = DeviceId::new();
        let domain = alloc_irq_domain(owner, IrqDomainKind::RiscvPlic).unwrap();

        assert_eq!(domain, IrqDomainId(7));
        assert_eq!(
            alloc_irq_domain(owner, IrqDomainKind::RiscvPlic),
            Ok(domain)
        );
        assert_eq!(domain_by_owner(owner).unwrap().id, domain);
        assert_eq!(domain_by_kind(IrqDomainKind::RiscvPlic).unwrap().id, domain);
    }

    #[test]
    fn register_irq_domain_rejects_owner_and_id_conflicts() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        let owner_a = DeviceId::new();
        let owner_b = DeviceId::new();
        let preferred = IrqDomainId(42);

        assert_eq!(
            register_irq_domain(owner_a, Some(preferred), IrqDomainKind::AArch64Gic),
            Ok(preferred)
        );
        assert_eq!(
            register_irq_domain(owner_a, Some(preferred), IrqDomainKind::RiscvPlic),
            Err(IrqError::Busy)
        );
        assert_eq!(
            register_irq_domain(owner_b, Some(preferred), IrqDomainKind::RiscvPlic),
            Err(IrqError::Busy)
        );
    }

    #[test]
    fn register_irq_domain_rejects_second_controller_of_same_kind() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        let owner_a = DeviceId::new();
        let owner_b = DeviceId::new();

        alloc_irq_domain(owner_a, IrqDomainKind::AArch64Gic).unwrap();
        assert_eq!(
            alloc_irq_domain(owner_b, IrqDomainKind::AArch64Gic),
            Err(IrqError::Unsupported)
        );
    }

    #[test]
    fn register_irq_domain_rejects_reserved_preferred_ids() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        for id in [
            IrqDomainId(0),
            X86_LAPIC_DOMAIN,
            CPU_LOCAL_IRQ_DOMAIN,
            X86_IOAPIC_DOMAIN,
            AARCH64_GIC_DOMAIN,
            RISCV_PLIC_DOMAIN,
            LOONGARCH_EIOINTC_DOMAIN,
            LOONGARCH_PCH_PIC_DOMAIN,
            IrqDomainId(u16::MAX),
        ] {
            assert_eq!(
                register_irq_domain(DeviceId::new(), Some(id), IrqDomainKind::X86IoApic),
                Err(IrqError::InvalidIrq)
            );
        }
    }
}
