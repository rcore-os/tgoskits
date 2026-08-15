use core::{
    hint::spin_loop,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering},
};

use log::trace;
use tock_registers::{LocalRegisterCopy, interfaces::*};

mod gicc;
mod gicd;
mod gich;

use gicc::CpuInterfaceReg;
use gicd::DistributorReg;
use gich::HypervisorRegs;

use crate::version::{IrqVecReadable, IrqVecWriteable};
pub use crate::{IntId, VirtAddr, define::Trigger};

/// GICv2 driver. (support GICv1)
pub struct Gic {
    gicd: VirtAddr,
    gicc: VirtAddr,
    gich: Option<HypervisorInterface>, // Optional for GICv2
    cpu_targets: Option<&'static CpuTargetMap>,
}

unsafe impl Send for Gic {}

/// Lock-free access to GICv2 Distributor state used by IRQ completion paths.
#[derive(Clone, Copy)]
pub struct DistributorOperations {
    gicd: *mut DistributorReg,
}

// SAFETY: the pointer names the immutable, permanently mapped GICD register
// block. Individual Distributor registers provide the required MMIO
// synchronization; this capability does not expose ordinary Rust memory.
unsafe impl Send for DistributorOperations {}
// SAFETY: see the `Send` implementation. Concurrent reads of GICD_ISPENDR are
// architecturally supported and do not create Rust aliases to mutable memory.
unsafe impl Sync for DistributorOperations {}

impl DistributorOperations {
    fn gicd(&self) -> &DistributorReg {
        // SAFETY: `Gic::distributor_operations` only constructs this capability
        // from the live GICD mapping established during platform discovery.
        unsafe { &*self.gicd }
    }

    /// Returns the Distributor pending state for one interrupt.
    pub fn is_pending(&self, intid: IntId) -> bool {
        self.gicd().ISPENDR.get_irq_bit(intid.into())
    }
}

pub struct HyperAddress {
    pub gich: VirtAddr,
    pub gicv: VirtAddr,
}

impl HyperAddress {
    pub fn new(gich: VirtAddr, gicv: VirtAddr) -> Self {
        Self { gich, gicv }
    }
}

impl Gic {
    /// `gicd`: Distributor register base address. `gicc`: CPU interface register base address.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the provided pointers are valid and point to the correct GICv2 registers.
    pub const unsafe fn new(gicd: VirtAddr, gicc: VirtAddr, hyper: Option<HyperAddress>) -> Self {
        Self {
            gicd,
            gicc,
            gich: match hyper {
                Some(addr) => Some(unsafe {
                    HypervisorInterface::new(addr.gich.as_ptr(), addr.gicv.as_ptr())
                }),
                None => None,
            },
            cpu_targets: None,
        }
    }

    /// Attaches the CPU-interface routing table populated during per-CPU init.
    pub fn set_cpu_target_map(&mut self, cpu_targets: &'static CpuTargetMap) {
        self.cpu_targets = Some(cpu_targets);
    }

    /// Returns the target bit associated with a host hardware CPU identifier.
    pub fn target_for_hardware_cpu(&self, hardware_cpu_id: usize) -> Option<TargetList> {
        self.cpu_targets?.for_hardware_cpu(hardware_cpu_id)
    }

    /// Returns the target capability associated with a host hardware CPU identifier.
    pub fn cpu_interface_target_for_hardware_cpu(
        &self,
        hardware_cpu_id: usize,
    ) -> Option<CpuInterfaceTarget> {
        self.cpu_targets?
            .cpu_interface_target_for_hardware_cpu(hardware_cpu_id)
    }

    fn gicd(&self) -> &DistributorReg {
        unsafe { &*(self.gicd.as_ptr()) }
    }

    pub fn gicc_addr(&self) -> VirtAddr {
        self.gicc
    }

    pub fn gicd_addr(&self) -> VirtAddr {
        self.gicd
    }

    /// Returns an IRQ-safe view of stable Distributor state.
    ///
    /// The returned capability does not own the register mapping. It remains
    /// valid for the lifetime of the initialized GIC driver.
    pub fn distributor_operations(&self) -> DistributorOperations {
        DistributorOperations {
            gicd: self.gicd.as_ptr(),
        }
    }

    pub fn max_intid(&self) -> u32 {
        self.gicd().max_spi_num()
    }

    pub fn cpu_interface(&self) -> CpuInterface {
        CpuInterface {
            gicd: self.gicd.as_ptr(),
            gicc: self.gicc.as_ptr(),
        }
    }

    pub fn hypervisor_interface(&self) -> Option<HypervisorInterface> {
        self.gich.as_ref().map(|h| HypervisorInterface {
            gich: h.gich,
            gicv: h.gicv,
        })
    }

    /// Initializes the GIC according to the GICv2 specification.
    ///
    /// This includes both Distributor and CPU Interface initialization. Use
    /// [`Self::try_init`] when target-discovery failures must be reported to a
    /// platform probe instead of treated as a one-time initialization failure.
    pub fn init(&mut self) {
        self.try_init()
            .unwrap_or_else(|error| panic!("failed to discover boot GICv2 CPU target: {error}"));
    }

    /// Discovers the boot CPU-interface target and initializes the GIC.
    ///
    /// Target discovery runs before the Distributor is disabled, so an error
    /// leaves the controller untouched.
    pub fn try_init(&mut self) -> Result<CpuInterfaceTarget, CpuTargetDiscoveryError> {
        let boot_target = self.cpu_interface().discover_target()?;
        self.init_with_target(boot_target);
        Ok(boot_target)
    }

    fn init_with_target(&mut self, boot_target: CpuInterfaceTarget) {
        trace!(
            "Initializing GICv2 Distributor@{:#p}...",
            self.gicd.as_ptr::<u8>()
        );
        // 1. Disable the Distributor first
        self.gicd().disable();

        // 2. Get the number of interrupt lines supported
        let max_spi = self.gicd().max_spi_num();

        // 3. Disable all interrupts first
        self.gicd().irq_disable_all(max_spi);

        // 4. Clear all pending interrupts
        self.gicd().pending_clear_all(max_spi);

        // 5. Clear all active interrupts
        self.gicd().active_clear_all(max_spi);

        // 6. Configure all interrupts as Group 1 (Non-secure) by default
        self.gicd().groups_all_to_0(max_spi);
        trace!("[GICv2] Configure all interrupts as Group 1 (Non-secure) by default");

        // 7. Set default priority for spi interrupts
        self.gicd().set_default_spi_priorities(max_spi);

        // 8. Configure interrupt targets (for SPIs)
        if let CpuInterfaceTarget::Explicit(target) = boot_target {
            self.gicd().configure_interrupt_targets(max_spi, target);
            trace!(
                "[GICv2] Configure all SPIs to target BSP mask {:#04x}",
                target.as_u8()
            );
        }
        // 9. Configure interrupt configuration (edge/level trigger)
        self.gicd().configure_interrupt_config(max_spi);

        // 10. Enable the Distributor
        self.gicd().enable();
    }

    /// Set interrupt enable state
    pub fn set_irq_enable(&self, intid: IntId, enable: bool) {
        if enable {
            self.gicd().ISENABLER.set_irq_bit(intid.into());
        } else {
            self.gicd().ICENABLER.set_irq_bit(intid.into());
        }
    }

    /// Is interrupt enabled?
    pub fn is_irq_enable(&self, id: IntId) -> bool {
        self.gicd().ISENABLER.get_irq_bit(id.into())
    }

    /// Set interrupt priority (0 = highest priority, 255 = lowest priority)
    pub fn set_priority(&self, id: IntId, priority: u8) {
        let index = id.to_u32() as usize;
        assert!(
            index < self.gicd().IPRIORITYR.len(),
            "Invalid interrupt ID for priority: {id:?}"
        );
        self.gicd().IPRIORITYR[index].set(priority);
    }

    pub fn get_priority(&self, id: IntId) -> u8 {
        let index = id.to_u32() as usize;
        assert!(
            index < self.gicd().IPRIORITYR.len(),
            "Invalid interrupt ID for priority: {id:?}"
        );
        self.gicd().IPRIORITYR[index].get()
    }

    /// Set interrupt target CPU for SPIs
    pub fn set_target_cpu(&self, id: IntId, target_list: TargetList) {
        assert!(
            !id.is_private(),
            "Cannot set target CPU for private interrupt: {id:?}"
        );
        let index = id.to_u32() as usize;
        assert!(
            index < self.gicd().ITARGETSR.len(),
            "Invalid interrupt ID for target: {id:?}"
        );
        self.gicd().ITARGETSR[index].set(target_list.as_u8());
    }

    /// Routes an SPI through the target capability discovered for one CPU interface.
    ///
    /// Uniprocessor GICs route SPIs to their sole CPU interface implicitly and
    /// expose `GICD_ITARGETSR` as RAZ/WI, so no register write is required.
    pub fn route_interrupt_to_cpu(&self, id: IntId, target: CpuInterfaceTarget) {
        if let CpuInterfaceTarget::Explicit(target) = target {
            self.set_target_cpu(id, target);
        }
    }

    pub fn get_target_cpu(&self, id: IntId) -> TargetList {
        assert!(
            !id.is_private(),
            "Cannot get target CPU for private interrupt: {id:?}"
        );
        let index = id.to_u32() as usize;
        assert!(
            index < self.gicd().ITARGETSR.len(),
            "Invalid interrupt ID for target: {id:?}"
        );
        TargetList(self.gicd().ITARGETSR[index].get())
    }

    /// Configure interrupt as Group 0 (Secure) or Group 1 (Non-secure)
    pub fn set_interrupt_group1(&self, id: IntId, group1: bool) {
        if group1 {
            self.gicd().IGROUPR.set_irq_bit(id.into());
        } else {
            self.gicd().IGROUPR.clear_irq_bit(id.into());
        }
    }

    /// Send a Software Generated Interrupt (SGI) to target CPUs
    ///
    /// # Arguments
    /// * `sgi_id` - SGI interrupt ID (0-15)
    /// * `target` - Target CPUs for the SGI
    pub fn send_sgi(&self, sgi_id: IntId, target: SGITarget) {
        let sgi_id = sgi_id.to_u32();
        assert!(sgi_id < 16, "Invalid SGI ID: {sgi_id}");
        let (filter, target_list) = match target {
            SGITarget::TargetList(list) => (
                gicd::SGIR::TargetListFilter::TargetList,
                list.as_u8() as u32,
            ),
            SGITarget::AllOther => (gicd::SGIR::TargetListFilter::AllOther, 0),
            SGITarget::Current => (gicd::SGIR::TargetListFilter::Current, 0),
        };

        self.gicd().SGIR.write(
            gicd::SGIR::SGIINTID.val(sgi_id) + gicd::SGIR::CPUTargetList.val(target_list) + filter,
        );
    }

    pub fn set_active(&self, id: IntId, active: bool) {
        if active {
            self.gicd().ISACTIVER.set_irq_bit(id.into());
        } else {
            self.gicd().ICACTIVER.set_irq_bit(id.into());
        }
    }

    pub fn is_active(&self, id: IntId) -> bool {
        self.gicd().ISACTIVER.get_irq_bit(id.into())
    }

    pub fn set_pending(&self, id: IntId, pending: bool) {
        if pending {
            self.gicd().ISPENDR.set_irq_bit(id.into());
        } else {
            self.gicd().ICPENDR.set_irq_bit(id.into());
        }
    }

    pub fn is_pending(&self, id: IntId) -> bool {
        self.gicd().ISPENDR.get_irq_bit(id.into())
    }

    pub fn gich_ref(&self) -> Option<&HypervisorInterface> {
        self.gich.as_ref()
    }

    pub fn iidr_raw(&self) -> u32 {
        self.gicd().IIDR.get()
    }

    pub fn typer_raw(&self) -> u32 {
        self.gicd().TYPER.get()
    }

    pub fn set_cfg(&self, id: IntId, cfg: Trigger) {
        self.gicd().set_cfg(id, cfg);
    }

    pub fn get_cfg(&self, id: IntId) -> Trigger {
        self.gicd().get_cfg(id)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SGITarget {
    /// Forward to CPUs listed in CPUTargetList (cpu mask)
    TargetList(TargetList),
    /// Forward to all CPUs except the requesting CPU
    AllOther,
    /// Forward only to the requesting CPU
    Current,
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TargetList(u8);

impl TargetList {
    /// Creates a target list from the CPU target mask reported by GICv2.
    pub const fn from_raw(raw: u8) -> Self {
        Self(raw)
    }

    /// Creates a target list only when the controller reported one interface bit.
    pub const fn from_one_hot(raw: u8) -> Option<Self> {
        if raw.count_ones() == 1 {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// Create a new TargetList with a specific CPU target list. list is Cpu interface IDs.
    pub fn new(list: impl Iterator<Item = usize>) -> Self {
        let mut raw = 0;
        for cpu in list {
            assert!(cpu < 8, "Invalid CPU Interface: {cpu}");
            raw |= 1 << cpu; // Set bit for each target CPU
        }
        Self(raw)
    }

    pub fn add(&mut self, cpu: usize) {
        assert!(cpu < 8, "Invalid CPU Interface: {cpu}");
        self.0 |= 1 << cpu; // Set bit for the target CPU
    }

    pub const fn as_u8(&self) -> u8 {
        self.0
    }

    pub fn cpu_id_list(&self) -> impl Iterator<Item = usize> {
        (0..8).filter(move |i| (self.0 & (1 << i)) != 0)
    }
}

/// The target-list capability exposed by one GICv2 CPU interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuInterfaceTarget {
    /// A uniprocessor controller makes ITARGETSR read-as-zero/write-ignored.
    ImplicitUniprocessor,
    /// The controller reports one implementation-defined target-list bit.
    Explicit(TargetList),
}

/// Failure to discover one usable GICv2 CPU-interface target.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CpuTargetDiscoveryError {
    /// The banked target registers exposed no unique interface bit.
    #[error("banked GICv2 target registers did not expose one unique CPU-interface bit")]
    MissingOrAmbiguous,
}

const MAX_CPU_INTERFACES: usize = 8;
const TARGET_UNPUBLISHED: u16 = 0;
const TARGET_IMPLICIT_UNIPROCESSOR: u16 = 1 << u8::BITS;
const TARGET_INITIALIZING: u16 = u16::MAX;

struct CpuTargetRoute {
    hardware_cpu_id: AtomicUsize,
    target: AtomicU16,
}

impl CpuTargetRoute {
    const fn empty() -> Self {
        Self {
            hardware_cpu_id: AtomicUsize::new(0),
            target: AtomicU16::new(TARGET_UNPUBLISHED),
        }
    }
}

/// Error returned when registering a GICv2 CPU-interface route.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CpuTargetMapError {
    /// The logical CPU does not fit the eight GICv2 target bits.
    #[error("logical CPU does not fit the eight GICv2 CPU interfaces")]
    InvalidLogicalCpu,
    /// A banked target must contain exactly one CPU-interface bit.
    #[error("GICv2 route must contain exactly one CPU-interface target")]
    InvalidTarget,
    /// The logical CPU, hardware CPU, or target bit is already mapped differently.
    #[error("GICv2 logical CPU, hardware CPU, or target is already mapped differently")]
    ConflictingRoute,
}

/// GICv2 CPU-interface routes discovered from banked private `GICD_ITARGETSR` values.
pub struct CpuTargetMap {
    registration_lock: AtomicBool,
    routes: [CpuTargetRoute; MAX_CPU_INTERFACES],
}

impl Default for CpuTargetMap {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuTargetMap {
    /// Creates an empty CPU route table.
    pub const fn new() -> Self {
        Self {
            registration_lock: AtomicBool::new(false),
            routes: [const { CpuTargetRoute::empty() }; MAX_CPU_INTERFACES],
        }
    }

    /// Records one logical/hardware CPU association with its GICv2 target bit.
    pub fn record(
        &self,
        logical_cpu: usize,
        hardware_cpu_id: usize,
        target: TargetList,
    ) -> Result<(), CpuTargetMapError> {
        self.record_cpu_interface_target(
            logical_cpu,
            hardware_cpu_id,
            CpuInterfaceTarget::Explicit(target),
        )
    }

    /// Records one logical/hardware CPU association with its target capability.
    pub fn record_cpu_interface_target(
        &self,
        logical_cpu: usize,
        hardware_cpu_id: usize,
        target: CpuInterfaceTarget,
    ) -> Result<(), CpuTargetMapError> {
        if logical_cpu >= MAX_CPU_INTERFACES {
            return Err(CpuTargetMapError::InvalidLogicalCpu);
        }
        let target_is_valid = match target {
            CpuInterfaceTarget::ImplicitUniprocessor => logical_cpu == 0,
            CpuInterfaceTarget::Explicit(target) => target.as_u8().is_power_of_two(),
        };
        if !target_is_valid {
            return Err(CpuTargetMapError::InvalidTarget);
        }
        let target = encode_cpu_target(target);

        while self
            .registration_lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        let result = self.record_locked(logical_cpu, hardware_cpu_id, target);
        self.registration_lock.store(false, Ordering::Release);
        result
    }

    fn record_locked(
        &self,
        logical_cpu: usize,
        hardware_cpu_id: usize,
        target: u16,
    ) -> Result<(), CpuTargetMapError> {
        for (index, route) in self.routes.iter().enumerate() {
            let existing_target = route.target.load(Ordering::Acquire);
            if existing_target == TARGET_UNPUBLISHED || existing_target == TARGET_INITIALIZING {
                continue;
            }
            let existing_hardware_cpu = route.hardware_cpu_id.load(Ordering::Relaxed);
            if index == logical_cpu {
                return if existing_hardware_cpu == hardware_cpu_id && existing_target == target {
                    Ok(())
                } else {
                    Err(CpuTargetMapError::ConflictingRoute)
                };
            }
            if existing_target == TARGET_IMPLICIT_UNIPROCESSOR
                || target == TARGET_IMPLICIT_UNIPROCESSOR
            {
                return Err(CpuTargetMapError::ConflictingRoute);
            }
            if existing_hardware_cpu == hardware_cpu_id || existing_target == target {
                return Err(CpuTargetMapError::ConflictingRoute);
            }
        }

        let route = &self.routes[logical_cpu];
        route.target.store(TARGET_INITIALIZING, Ordering::Relaxed);
        route
            .hardware_cpu_id
            .store(hardware_cpu_id, Ordering::Relaxed);
        route.target.store(target, Ordering::Release);
        Ok(())
    }

    /// Looks up the target bit for a host logical CPU index.
    pub fn for_logical_cpu(&self, logical_cpu: usize) -> Option<TargetList> {
        match self.cpu_interface_target_for_logical_cpu(logical_cpu)? {
            CpuInterfaceTarget::Explicit(target) => Some(target),
            CpuInterfaceTarget::ImplicitUniprocessor => None,
        }
    }

    /// Looks up the target capability for a host logical CPU index.
    pub fn cpu_interface_target_for_logical_cpu(
        &self,
        logical_cpu: usize,
    ) -> Option<CpuInterfaceTarget> {
        let target = self.routes.get(logical_cpu)?.target.load(Ordering::Acquire);
        decode_cpu_target(target)
    }

    /// Looks up the target bit for a host hardware CPU identifier.
    pub fn for_hardware_cpu(&self, hardware_cpu_id: usize) -> Option<TargetList> {
        match self.cpu_interface_target_for_hardware_cpu(hardware_cpu_id)? {
            CpuInterfaceTarget::Explicit(target) => Some(target),
            CpuInterfaceTarget::ImplicitUniprocessor => None,
        }
    }

    /// Looks up the target capability for a host hardware CPU identifier.
    pub fn cpu_interface_target_for_hardware_cpu(
        &self,
        hardware_cpu_id: usize,
    ) -> Option<CpuInterfaceTarget> {
        self.routes.iter().find_map(|route| {
            let target = route.target.load(Ordering::Acquire);
            if route.hardware_cpu_id.load(Ordering::Relaxed) != hardware_cpu_id {
                return None;
            }
            decode_cpu_target(target)
        })
    }
}

const fn encode_cpu_target(target: CpuInterfaceTarget) -> u16 {
    match target {
        CpuInterfaceTarget::ImplicitUniprocessor => TARGET_IMPLICIT_UNIPROCESSOR,
        CpuInterfaceTarget::Explicit(target) => target.as_u8() as u16,
    }
}

const fn decode_cpu_target(raw: u16) -> Option<CpuInterfaceTarget> {
    if raw == TARGET_IMPLICIT_UNIPROCESSOR {
        return Some(CpuInterfaceTarget::ImplicitUniprocessor);
    }
    if raw > u8::MAX as u16 {
        return None;
    }
    let raw = raw as u8;
    match TargetList::from_one_hot(raw) {
        Some(target) => Some(CpuInterfaceTarget::Explicit(target)),
        None => None,
    }
}

impl SGITarget {
    /// Create a new SGITarget with a specific CPU target list. list is Cpu interface IDs.
    pub fn new_target_list(val: TargetList) -> Self {
        Self::TargetList(val)
    }
}

#[cfg(test)]
mod cpu_target_map_tests {
    use super::{CpuInterfaceTarget, CpuTargetMap, CpuTargetMapError, TargetList};

    #[test]
    fn records_and_looks_up_logical_and_hardware_cpu_routes() {
        let routes = CpuTargetMap::new();
        let target = TargetList::from_raw(0x40);

        routes.record(5, 0x102, target).unwrap();

        assert_eq!(routes.for_logical_cpu(5), Some(target));
        assert_eq!(routes.for_hardware_cpu(0x102), Some(target));
        assert_eq!(
            routes.cpu_interface_target_for_logical_cpu(5),
            Some(CpuInterfaceTarget::Explicit(target))
        );
        assert!(routes.for_logical_cpu(4).is_none());
        assert!(routes.for_hardware_cpu(0x103).is_none());
    }

    #[test]
    fn rejects_invalid_and_conflicting_routes_but_accepts_replay() {
        let routes = CpuTargetMap::new();

        assert_eq!(
            routes.record(0, 0, TargetList::from_raw(0)),
            Err(CpuTargetMapError::InvalidTarget)
        );
        assert_eq!(
            routes.record(0, 0, TargetList::from_raw(3)),
            Err(CpuTargetMapError::InvalidTarget)
        );
        assert_eq!(
            routes.record(8, 0, TargetList::from_raw(1)),
            Err(CpuTargetMapError::InvalidLogicalCpu)
        );

        let target_80 = TargetList::from_raw(0x80);
        routes.record(2, 0x102, target_80).unwrap();
        routes.record(2, 0x102, target_80).unwrap();
        assert_eq!(
            routes.record(2, 0x103, target_80),
            Err(CpuTargetMapError::ConflictingRoute)
        );
        assert_eq!(
            routes.record(3, 0x102, TargetList::from_raw(0x20)),
            Err(CpuTargetMapError::ConflictingRoute)
        );
        assert_eq!(
            routes.record(3, 0x103, target_80),
            Err(CpuTargetMapError::ConflictingRoute)
        );
    }

    #[test]
    fn records_only_one_implicit_uniprocessor_route() {
        let routes = CpuTargetMap::new();

        routes
            .record_cpu_interface_target(0, 0, CpuInterfaceTarget::ImplicitUniprocessor)
            .unwrap();
        assert_eq!(
            routes.cpu_interface_target_for_logical_cpu(0),
            Some(CpuInterfaceTarget::ImplicitUniprocessor)
        );
        assert_eq!(
            routes.cpu_interface_target_for_hardware_cpu(0),
            Some(CpuInterfaceTarget::ImplicitUniprocessor)
        );
        assert!(routes.for_logical_cpu(0).is_none());
        assert!(routes.for_hardware_cpu(0).is_none());
        assert_eq!(
            routes.record_cpu_interface_target(1, 1, CpuInterfaceTarget::ImplicitUniprocessor),
            Err(CpuTargetMapError::InvalidTarget)
        );
        assert_eq!(
            routes.record(1, 1, TargetList::from_raw(0x02)),
            Err(CpuTargetMapError::ConflictingRoute)
        );

        let explicit_routes = CpuTargetMap::new();
        explicit_routes
            .record(1, 1, TargetList::from_raw(0x02))
            .unwrap();
        assert_eq!(
            explicit_routes.record_cpu_interface_target(
                0,
                0,
                CpuInterfaceTarget::ImplicitUniprocessor
            ),
            Err(CpuTargetMapError::ConflictingRoute)
        );
    }
}
#[derive(Debug, Clone, Copy)]
pub enum Ack {
    SGI { intid: IntId, cpu_id: usize },
    Other(IntId),
}

impl Ack {
    pub fn is_special(&self) -> bool {
        if let Ack::Other(intid) = self {
            intid.is_special()
        } else {
            false
        }
    }
}

impl From<Ack> for u32 {
    fn from(ack: Ack) -> Self {
        match ack {
            Ack::Other(intid) => gicc::IAR::InterruptID.val(intid.to_u32()),
            Ack::SGI { intid, cpu_id } => {
                gicc::IAR::InterruptID.val(intid.to_u32()) + gicc::IAR::CPUID.val(cpu_id as u32)
            }
        }
        .value
    }
}

impl From<u32> for Ack {
    fn from(value: u32) -> Self {
        let reg = LocalRegisterCopy::<u32, gicc::IAR::Register>::new(value);
        let intid = unsafe { IntId::raw(reg.read(gicc::IAR::InterruptID)) };
        if intid.is_sgi() {
            let cpu_id = reg.read(gicc::IAR::CPUID) as usize;
            Ack::SGI { intid, cpu_id }
        } else {
            Ack::Other(intid)
        }
    }
}

/// Every CPU interface has its own GICC registers
pub struct CpuInterface {
    gicd: *mut DistributorReg,
    gicc: *mut CpuInterfaceReg,
}

unsafe impl Send for CpuInterface {}

impl CpuInterface {
    fn gicc(&self) -> &CpuInterfaceReg {
        unsafe { &*self.gicc }
    }

    fn gicd(&self) -> &DistributorReg {
        unsafe { &*self.gicd }
    }

    /// Returns the banked CPU target mask for the current CPU interface.
    pub fn current_cpu_target(&self) -> TargetList {
        // The register view is byte-addressed: indices 0..32 cover Linux's
        // eight banked 32-bit ITARGETSR registers; SPI targets begin at 32.
        TargetList::from_raw(
            self.gicd().ITARGETSR[..32]
                .iter()
                .fold(0, |mask, target| mask | target.get()),
        )
    }

    /// Discovers the current CPU target without inventing a bit for RAZ/WI UP GICs.
    pub fn discover_target(&self) -> Result<CpuInterfaceTarget, CpuTargetDiscoveryError> {
        let observed = self.current_cpu_target().as_u8();
        if let Some(target) = TargetList::from_one_hot(observed) {
            Ok(CpuInterfaceTarget::Explicit(target))
        } else if observed == 0 && self.gicd().cpu_interface_count() == 1 {
            Ok(CpuInterfaceTarget::ImplicitUniprocessor)
        } else {
            Err(CpuTargetDiscoveryError::MissingOrAmbiguous)
        }
    }

    /// Initialize the CPU interface for the current CPU
    pub fn init_current_cpu(&mut self) {
        let gicc = self.gicc();

        // 1. Disable CPU interface first
        gicc.CTLR.set(0);

        // 2. Set priority mask to allow all interrupts (lowest priority)
        gicc.PMR.write(gicc::PMR::Priority.val(0xFF));

        // // 3. Set binary point to default value (no preemption)
        // gicc.BPR.write(BPR::BinaryPoint.val(0x2));

        // // 4. Set aliased binary point for Group 1 interrupts
        // gicc.ABPR.write(ABPR::BinaryPoint.val(0x3));

        // 5. Enable CPU interface for both Group 0 and Group 1 interrupts
        gicc.CTLR.write(gicc::CTLR::EnableGrp0::SET);

        // 6. Set default priority for sgi and ppi interrupts
        self.gicd().set_default_sgi_ppi_priorities();
    }
    /// Set the EOI mode for non-secure interrupts
    ///
    /// - `false` GICC_EOIR has both priority drop and deactivate interrupt functionality. Accesses to the GICC_DIR are UNPREDICTABLE.
    /// - `true`  GICC_EOIR has priority drop functionality only. GICC_DIR has deactivate interrupt functionality.
    pub fn set_eoi_mode_ns(&self, is_two_step: bool) {
        if is_two_step {
            self.gicc().CTLR.modify(gicc::CTLR::EOImodeNS::SET);
        } else {
            self.gicc().CTLR.modify(gicc::CTLR::EOImodeNS::CLEAR);
        };
    }

    pub fn eoi_mode_ns(&self) -> bool {
        self.gicc().CTLR.is_set(gicc::CTLR::EOImodeNS)
    }

    /// Acknowledge an interrupt and return the interrupt ID
    /// Returns the interrupt ID and source CPU ID (for SGIs)
    pub fn ack(&self) -> Ack {
        self.gicc().IAR.get().into()
    }

    /// Signal end of interrupt processing
    pub fn eoi(&self, ack: Ack) {
        let val = match ack {
            Ack::Other(intid) => gicc::EOIR::EOIINTID.val(intid.to_u32()),
            Ack::SGI { intid, cpu_id } => {
                gicc::EOIR::EOIINTID.val(intid.to_u32()) + gicc::EOIR::CPUID.val(cpu_id as u32)
            }
        };
        self.gicc().EOIR.write(val);
    }

    /// Deactivate an interrupt
    pub fn dir(&self, ack: Ack) {
        let val = match ack {
            Ack::Other(intid) => gicc::DIR::InterruptID.val(intid.to_u32()),
            Ack::SGI { intid, cpu_id } => {
                gicc::DIR::InterruptID.val(intid.to_u32()) + gicc::DIR::CPUID.val(cpu_id as u32)
            }
        };
        self.gicc().DIR.write(val);
    }

    /// Get the highest priority pending interrupt ID
    pub fn get_highest_priority_pending(&self) -> u32 {
        let hppir = self.gicc().HPPIR.get();
        hppir & 0x3FF // Bits [9:0]
    }

    /// Get the current running priority
    pub fn get_running_priority(&self) -> u8 {
        (self.gicc().RPR.get() & 0xFF) as u8
    }

    /// Set the priority mask (interrupts with priority >= mask will be masked)
    pub fn set_priority_mask(&self, mask: u8) {
        self.gicc().PMR.write(gicc::PMR::Priority.val(mask as u32));
    }

    pub fn set_irq_enable(&self, id: IntId, enable: bool) {
        assert!(
            id.is_private(),
            "Cannot enable non-private interrupt: {id:?}"
        );
        if enable {
            self.gicd().ISENABLER.set_irq_bit(id.into());
        } else {
            self.gicd().ICENABLER.set_irq_bit(id.into());
        }
    }

    pub fn is_irq_enable(&self, id: IntId) -> bool {
        assert!(
            id.is_private(),
            "Cannot check non-private interrupt: {id:?}"
        );
        self.gicd().ISENABLER.get_irq_bit(id.into())
    }

    /// Set interrupt priority (0 = highest priority, 255 = lowest priority)
    pub fn set_priority(&self, id: IntId, priority: u8) {
        assert!(
            id.is_private(),
            "Cannot set priority for non-private interrupt: {id:?}"
        );
        let index = id.to_u32() as usize;
        assert!(
            index < self.gicd().IPRIORITYR.len(),
            "Invalid interrupt ID for priority: {id:?}"
        );
        self.gicd().IPRIORITYR[index].set(priority);
    }

    pub fn get_priority(&self, id: IntId) -> u8 {
        assert!(
            id.is_private(),
            "Cannot get priority for non-private interrupt: {id:?}"
        );
        let index = id.to_u32() as usize;
        assert!(
            index < self.gicd().IPRIORITYR.len(),
            "Invalid interrupt ID for priority: {id:?}"
        );
        self.gicd().IPRIORITYR[index].get()
    }

    pub fn set_active(&self, id: IntId, active: bool) {
        assert!(
            id.is_private(),
            "Cannot set active state for non-private interrupt: {id:?}"
        );
        if active {
            self.gicd().ISACTIVER.set_irq_bit(id.into());
        } else {
            self.gicd().ICACTIVER.set_irq_bit(id.into());
        }
    }

    pub fn is_active(&self, id: IntId) -> bool {
        assert!(
            id.is_private(),
            "Cannot check active state for non-private interrupt: {id:?}"
        );
        self.gicd().ISACTIVER.get_irq_bit(id.into())
    }

    pub fn set_pending(&self, id: IntId, pending: bool) {
        assert!(
            id.is_private(),
            "Cannot set pending state for non-private interrupt: {id:?}"
        );
        if pending {
            self.gicd().ISPENDR.set_irq_bit(id.into());
        } else {
            self.gicd().ICPENDR.set_irq_bit(id.into());
        }
    }

    pub fn is_pending(&self, id: IntId) -> bool {
        assert!(
            id.is_private(),
            "Cannot check pending state for non-private interrupt: {id:?}"
        );
        self.gicd().ISPENDR.get_irq_bit(id.into())
    }

    /// Send a Software Generated Interrupt (SGI) to target CPUs.
    pub fn send_sgi(&self, sgi_id: IntId, target: SGITarget) {
        let sgi_id = sgi_id.to_u32();
        assert!(sgi_id < 16, "Invalid SGI ID: {sgi_id}");
        let (filter, target_list) = match target {
            SGITarget::TargetList(list) => (
                gicd::SGIR::TargetListFilter::TargetList,
                list.as_u8() as u32,
            ),
            SGITarget::AllOther => (gicd::SGIR::TargetListFilter::AllOther, 0),
            SGITarget::Current => (gicd::SGIR::TargetListFilter::Current, 0),
        };

        self.gicd().SGIR.write(
            gicd::SGIR::SGIINTID.val(sgi_id) + gicd::SGIR::CPUTargetList.val(target_list) + filter,
        );
    }

    pub fn set_cfg(&self, id: IntId, trigger: Trigger) {
        self.gicd().set_cfg(id, trigger);
    }

    pub fn get_cfg(&self, id: IntId) -> Trigger {
        self.gicd().get_cfg(id)
    }

    pub const fn trap_operations(&self) -> TrapOp {
        TrapOp::new(self.gicc as *mut u8)
    }
}

pub struct TrapOp {
    gicc: *mut CpuInterfaceReg,
}

unsafe impl Send for TrapOp {}
unsafe impl Sync for TrapOp {}

impl TrapOp {
    const fn new(gicc: *mut u8) -> Self {
        Self { gicc: gicc as _ }
    }

    fn gicc(&self) -> &CpuInterfaceReg {
        unsafe { &*self.gicc }
    }

    pub fn eoi_mode_ns(&self) -> bool {
        self.gicc().CTLR.is_set(gicc::CTLR::EOImodeNS)
    }

    /// Acknowledge an interrupt and return the interrupt ID
    /// Returns the interrupt ID and source CPU ID (for SGIs)
    pub fn ack(&self) -> Ack {
        self.gicc().IAR.get().into()
    }

    /// Signal end of interrupt processing
    pub fn eoi(&self, ack: Ack) {
        let val = match ack {
            Ack::Other(intid) => gicc::EOIR::EOIINTID.val(intid.to_u32()),
            Ack::SGI { intid, cpu_id } => {
                gicc::EOIR::EOIINTID.val(intid.to_u32()) + gicc::EOIR::CPUID.val(cpu_id as u32)
            }
        };
        self.gicc().EOIR.write(val);
    }

    /// Deactivate an interrupt
    pub fn dir(&self, ack: Ack) {
        let val = match ack {
            Ack::Other(intid) => gicc::DIR::InterruptID.val(intid.to_u32()),
            Ack::SGI { intid, cpu_id } => {
                gicc::DIR::InterruptID.val(intid.to_u32()) + gicc::DIR::CPUID.val(cpu_id as u32)
            }
        };
        self.gicc().DIR.write(val);
    }
}

/// GIC Hypervisor Interface for virtualization support
pub struct HypervisorInterface {
    gich: *mut HypervisorRegs,
    gicv: *mut CpuInterfaceReg,
}

unsafe impl Send for HypervisorInterface {}

impl HypervisorInterface {
    /// Create a new HypervisorInterface
    ///
    /// # Safety
    ///
    /// The caller must ensure that the provided pointer is valid and points to the correct GICH registers.
    pub const unsafe fn new(gich: *mut u8, gicv: *mut u8) -> Self {
        Self {
            gich: gich as _,
            gicv: gicv as _,
        }
    }

    fn gich(&self) -> &HypervisorRegs {
        unsafe { &*self.gich }
    }

    fn gicv(&self) -> &CpuInterfaceReg {
        unsafe { &*self.gicv }
    }

    /// Initialize the hypervisor interface
    pub fn init_current_cpu(&mut self) {
        let gich = self.gich();

        // Disable the hypervisor interface first
        gich.HCR.set(0);

        // Clear all list registers
        for lr in &gich.LR {
            lr.set(0);
        }

        // Clear active priorities
        gich.APR.set(0);
    }

    pub fn gicv_address(&self) -> NonNull<u8> {
        unsafe { NonNull::new_unchecked(self.gicv as *mut u8) }
    }

    /// Enable the virtual CPU interface
    pub fn enable(&self) {
        self.gich().HCR.modify(gich::HCR::En::SET);
    }

    /// Disable the virtual CPU interface
    pub fn disable(&self) {
        self.gich().HCR.modify(gich::HCR::En::CLEAR);
    }

    /// Enable/disable underflow maintenance interrupt
    pub fn set_underflow_interrupt(&self, enable: bool) {
        if enable {
            self.gich().HCR.modify(gich::HCR::UIE::SET);
        } else {
            self.gich().HCR.modify(gich::HCR::UIE::CLEAR);
        }
    }

    /// Enable/disable list register entry not present maintenance interrupt
    pub fn set_list_reg_entry_not_present_interrupt(&self, enable: bool) {
        if enable {
            self.gich().HCR.modify(gich::HCR::LRENPIE::SET);
        } else {
            self.gich().HCR.modify(gich::HCR::LRENPIE::CLEAR);
        }
    }

    /// Enable/disable no pending maintenance interrupt
    pub fn set_no_pending_interrupt(&self, enable: bool) {
        if enable {
            self.gich().HCR.modify(gich::HCR::NPIE::SET);
        } else {
            self.gich().HCR.modify(gich::HCR::NPIE::CLEAR);
        }
    }

    /// Enable/disable virtual Group 0 enable maintenance interrupt
    pub fn set_vgrp0_enable_interrupt(&self, enable: bool) {
        if enable {
            self.gich().HCR.modify(gich::HCR::VGrp0EIE::SET);
        } else {
            self.gich().HCR.modify(gich::HCR::VGrp0EIE::CLEAR);
        }
    }

    /// Enable/disable virtual Group 0 disable maintenance interrupt
    pub fn set_vgrp0_disable_interrupt(&self, enable: bool) {
        if enable {
            self.gich().HCR.modify(gich::HCR::VGrp0DIE::SET);
        } else {
            self.gich().HCR.modify(gich::HCR::VGrp0DIE::CLEAR);
        }
    }

    /// Enable/disable virtual Group 1 enable maintenance interrupt
    pub fn set_vgrp1_enable_interrupt(&self, enable: bool) {
        if enable {
            self.gich().HCR.modify(gich::HCR::VGrp1EIE::SET);
        } else {
            self.gich().HCR.modify(gich::HCR::VGrp1EIE::CLEAR);
        }
    }

    /// Enable/disable virtual Group 1 disable maintenance interrupt
    pub fn set_vgrp1_disable_interrupt(&self, enable: bool) {
        if enable {
            self.gich().HCR.modify(gich::HCR::VGrp1DIE::SET);
        } else {
            self.gich().HCR.modify(gich::HCR::VGrp1DIE::CLEAR);
        }
    }

    /// Set a virtual interrupt in a list register
    pub fn set_virtual_interrupt(&self, lr_index: usize, config: VirtualInterruptConfig) {
        assert!(lr_index < 64, "Invalid list register index");

        let mut lr_val = gich::LR::VirtualID.val(config.virtual_id.to_u32())
            + gich::LR::Priority.val(config.priority as u32)
            + gich::LR::State.val(config.state as u32);

        if config.group1 {
            lr_val += gich::LR::Grp1::SET;
        }

        match config.interrupt_type {
            VirtualInterruptType::Hardware { physical_id } => {
                lr_val += gich::LR::HW::SET + gich::LR::PhysicalID.val(physical_id);
            }
            VirtualInterruptType::Software {
                cpu_id,
                eoi_maintenance,
            } => {
                // if is not sgi, cpu_id must be 0
                if let Some(cpu_id) = cpu_id
                    && config.virtual_id.is_sgi()
                {
                    lr_val += gich::LR::CPUID.val(cpu_id as u32);
                }
                if eoi_maintenance {
                    lr_val += gich::LR::EOI::SET;
                }
            }
        }

        self.gich().LR[lr_index].write(lr_val);
    }

    /// Get a virtual interrupt configuration from a list register
    pub fn get_virtual_interrupt(&self, lr_index: usize) -> VirtualInterruptConfig {
        assert!(lr_index < 64, "Invalid list register index");

        let lr_val = self.gich().LR[lr_index].extract();

        // Extract virtual interrupt ID
        let virtual_id = unsafe { IntId::raw(lr_val.read(gich::LR::VirtualID)) };

        // Extract priority (5 bits, but stored in u8)
        let priority = (lr_val.read(gich::LR::Priority) << 3) as u8; // Shift to make it 8-bit priority

        // Extract state
        let state_val = lr_val.read(gich::LR::State);
        let state = match state_val {
            1 => VirtualInterruptState::Pending,
            2 => VirtualInterruptState::Active,
            3 => VirtualInterruptState::PendingAndActive,
            _ => VirtualInterruptState::Invalid, // Fallback for invalid values
        };

        // Extract group
        let group1 = lr_val.is_set(gich::LR::Grp1);

        // Extract hardware interrupt flag and create appropriate interrupt type
        let interrupt_type = if lr_val.is_set(gich::LR::HW) {
            // Hardware interrupt
            let physical_id = lr_val.read(gich::LR::PhysicalID);
            VirtualInterruptType::Hardware { physical_id }
        } else {
            // Software interrupt
            let cpu_id_val = lr_val.read(gich::LR::CPUID);
            let cpu_id = if cpu_id_val != 0 {
                Some(cpu_id_val as usize)
            } else {
                None
            };
            let eoi_maintenance = lr_val.is_set(gich::LR::EOI);
            VirtualInterruptType::Software {
                cpu_id,
                eoi_maintenance,
            }
        };

        VirtualInterruptConfig {
            virtual_id,
            priority,
            state,
            group1,
            interrupt_type,
        }
    }

    /// Check if a list register is empty (invalid state)
    pub fn is_list_register_empty(&self, lr_index: usize) -> bool {
        if lr_index >= 64 {
            return true; // Invalid index is considered empty
        }

        let lr_val = self.gich().LR[lr_index].extract();
        let state_val = lr_val.read(gich::LR::State);
        state_val == 0 // Invalid state means empty
    }

    /// Clear a list register (set to invalid state)
    pub fn clear_list_register(&self, lr_index: usize) -> Result<(), &'static str> {
        if lr_index >= 64 {
            return Err("Invalid list register index");
        }

        self.gich().LR[lr_index].set(0);
        Ok(())
    }

    /// Get the maintenance interrupt status
    pub fn get_maintenance_status(&self) -> u32 {
        self.gich().MISR.get()
    }

    /// Get the number of implemented list registers
    pub fn get_list_register_count(&self) -> usize {
        (self.gich().VTR.read(gich::VTR::ListRegs) + 1) as usize
    }

    /// Returns the number of implemented priority bits.
    pub fn priority_bits(&self) -> u8 {
        (self.gich().VTR.read(gich::VTR::PRIbits) + 1) as u8
    }

    /// Returns the raw GICH_HCR value for vCPU context save.
    pub fn hcr_raw(&self) -> u32 {
        self.gich().HCR.get()
    }

    /// Restores a raw GICH_HCR value for vCPU context load.
    pub fn set_hcr_raw(&self, value: u32) {
        self.gich().HCR.set(value);
    }

    /// Returns the raw GICH_VMCR value for vCPU context save.
    pub fn vmcr_raw(&self) -> u32 {
        self.gich().VMCR.get()
    }

    /// Restores a raw GICH_VMCR value for vCPU context load.
    pub fn set_vmcr_raw(&self, value: u32) {
        self.gich().VMCR.set(value);
    }

    /// Returns the raw GICH_APR value for vCPU context save.
    pub fn apr_raw(&self) -> u32 {
        self.gich().APR.get()
    }

    /// Restores the raw GICH_APR value for vCPU context load.
    pub fn set_apr_raw(&self, value: u32) {
        self.gich().APR.set(value);
    }

    /// Returns one raw GICH_LR value after validating the hardware index.
    pub fn list_register_raw(&self, index: usize) -> Option<u32> {
        (index < self.get_list_register_count()).then(|| self.gich().LR[index].get())
    }

    /// Restores one raw GICH_LR value after validating the hardware index.
    pub fn set_list_register_raw(&self, index: usize, value: u32) -> Result<(), &'static str> {
        if index >= self.get_list_register_count() {
            return Err("list register index exceeds the implemented GICH range");
        }
        self.gich().LR[index].set(value);
        Ok(())
    }

    /// Get EOI status registers
    pub fn get_eoi_status(&self) -> (u32, u32) {
        (self.gich().EISR0.get(), self.gich().EISR1.get())
    }

    /// Get empty list register status
    pub fn get_empty_lr_status(&self) -> (u32, u32) {
        (self.gich().ELRSR0.get(), self.gich().ELRSR1.get())
    }

    pub fn gicv_aiar(&self) -> Option<Ack> {
        let data = self.gicv().AIAR.extract();
        let id = data.read(gicc::AIAR::InterruptID);
        if id == 1023 {
            return None;
        }
        Some(data.get().into())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VirtualInterruptConfig {
    pub virtual_id: IntId,
    pub priority: u8,
    pub state: VirtualInterruptState,
    pub group1: bool,
    pub interrupt_type: VirtualInterruptType,
}

impl VirtualInterruptConfig {
    /// Create a new virtual interrupt configuration
    pub fn new(
        virtual_id: IntId,
        priority: u8,
        state: VirtualInterruptState,
        group1: bool,
        interrupt_type: VirtualInterruptType,
    ) -> Self {
        Self {
            virtual_id,
            priority,
            state,
            group1,
            interrupt_type,
        }
    }

    /// Create a hardware virtual interrupt configuration
    pub fn hardware(
        virtual_id: IntId,
        physical_id: u32,
        priority: u8,
        state: VirtualInterruptState,
        group1: bool,
    ) -> Self {
        Self::new(
            virtual_id,
            priority,
            state,
            group1,
            VirtualInterruptType::hardware(physical_id),
        )
    }

    /// Create a software virtual interrupt configuration
    pub fn software(
        virtual_id: IntId,
        cpu_id: Option<usize>,
        priority: u8,
        state: VirtualInterruptState,
        group1: bool,
        eoi_maintenance: bool,
    ) -> Self {
        Self::new(
            virtual_id,
            priority,
            state,
            group1,
            VirtualInterruptType::software(cpu_id, eoi_maintenance),
        )
    }
}

/// Virtual interrupt type for List Register configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualInterruptType {
    /// Software interrupt - uses CPU ID and optional EOI maintenance
    Software {
        cpu_id: Option<usize>,
        eoi_maintenance: bool,
    },
    /// Hardware interrupt - uses physical interrupt ID
    Hardware { physical_id: u32 },
}

impl VirtualInterruptType {
    /// Create a software interrupt type
    pub fn software(cpu_id: Option<usize>, eoi_maintenance: bool) -> Self {
        Self::Software {
            cpu_id,
            eoi_maintenance,
        }
    }

    /// Create a hardware interrupt type
    pub fn hardware(physical_id: u32) -> Self {
        Self::Hardware { physical_id }
    }

    /// Check if this is a hardware interrupt
    pub fn is_hardware(&self) -> bool {
        matches!(self, Self::Hardware { .. })
    }

    /// Check if this is a software interrupt
    pub fn is_software(&self) -> bool {
        matches!(self, Self::Software { .. })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum VirtualInterruptState {
    Invalid          = 0,
    Pending          = 1,
    Active           = 2,
    PendingAndActive = 3,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn target_list_preserves_banked_cpu_target_mask() {
        let target = TargetList::from_raw(0x20);

        assert_eq!(target.as_u8(), 0x20);
        assert_eq!(target.cpu_id_list().collect::<std::vec::Vec<_>>(), [5]);
    }

    #[test]
    fn cpu_interface_scans_every_banked_target_byte() {
        let mut distributor = std::boxed::Box::new([0u8; 0x1000]);
        let mut cpu_registers = std::boxed::Box::new([0u8; 0x1000]);
        let cpu = CpuInterface {
            gicd: distributor.as_mut_ptr().cast(),
            gicc: cpu_registers.as_mut_ptr().cast(),
        };
        cpu.gicd().ITARGETSR[0].set(0);
        cpu.gicd().ITARGETSR[31].set(0x40);
        cpu.gicd().ITARGETSR[32].set(0x02);

        assert_eq!(cpu.current_cpu_target().as_u8(), 0x40);
    }

    #[test]
    fn cpu_target_discovery_distinguishes_implicit_up_from_explicit_smp() {
        let mut distributor = std::boxed::Box::new([0u8; 0x1000]);
        let mut cpu_registers = std::boxed::Box::new([0u8; 0x1000]);
        let cpu = CpuInterface {
            gicd: distributor.as_mut_ptr().cast(),
            gicc: cpu_registers.as_mut_ptr().cast(),
        };

        assert_eq!(
            cpu.discover_target(),
            Ok(CpuInterfaceTarget::ImplicitUniprocessor)
        );
        distributor[4..8].copy_from_slice(&(1u32 << 5).to_ne_bytes());
        assert_eq!(
            cpu.discover_target(),
            Err(CpuTargetDiscoveryError::MissingOrAmbiguous)
        );

        cpu.gicd().ITARGETSR[29].set(0x04);
        assert_eq!(
            cpu.discover_target(),
            Ok(CpuInterfaceTarget::Explicit(TargetList::from_raw(0x04)))
        );
        cpu.gicd().ITARGETSR[31].set(0x02);
        assert_eq!(
            cpu.discover_target(),
            Err(CpuTargetDiscoveryError::MissingOrAmbiguous)
        );
    }

    #[test]
    fn distributor_initializes_spis_with_bsp_target_mask() {
        let mut regs = std::boxed::Box::new([0u8; 0x1000]);
        regs[4..8].copy_from_slice(&1u32.to_ne_bytes());
        regs[0x800] = 0x40;
        let mut gic = unsafe {
            Gic::new(
                VirtAddr::from(regs.as_mut_ptr()),
                VirtAddr::from(regs.as_mut_ptr()),
                None,
            )
        };

        let bsp_target = TargetList::from_raw(0x40);
        assert_eq!(gic.try_init(), Ok(CpuInterfaceTarget::Explicit(bsp_target)));

        let regs = gic.gicd();
        assert_eq!(regs.IPRIORITYR[31].get(), 0);
        assert_eq!(regs.IPRIORITYR[32].get(), 0xA0);
        assert_eq!(regs.IPRIORITYR[63].get(), 0xA0);
        assert_eq!(regs.IPRIORITYR[64].get(), 0);

        assert_eq!(regs.ITARGETSR[31].get(), 0);
        assert_eq!(regs.ITARGETSR[32].get(), bsp_target.as_u8());
        assert_eq!(regs.ITARGETSR[63].get(), bsp_target.as_u8());
        assert_eq!(regs.ITARGETSR[64].get(), 0);
    }

    #[test]
    fn distributor_leaves_implicit_uniprocessor_targets_untouched() {
        let mut regs = std::boxed::Box::new([0u8; 0x1000]);
        regs[4..8].copy_from_slice(&1u32.to_ne_bytes());
        regs[0x820] = 0x55;
        let mut gic = unsafe {
            Gic::new(
                VirtAddr::from(regs.as_mut_ptr()),
                VirtAddr::from(regs.as_mut_ptr()),
                None,
            )
        };

        gic.init();

        assert_eq!(gic.gicd().ITARGETSR[32].get(), 0x55);
    }

    #[test]
    fn interrupt_routing_reuses_explicit_and_implicit_target_semantics() {
        let mut regs = std::boxed::Box::new([0u8; 0x1000]);
        let gic = unsafe {
            Gic::new(
                VirtAddr::from(regs.as_mut_ptr()),
                VirtAddr::from(regs.as_mut_ptr()),
                None,
            )
        };
        let intid = IntId::spi(0);
        gic.gicd().ITARGETSR[32].set(0x55);

        gic.route_interrupt_to_cpu(intid, CpuInterfaceTarget::ImplicitUniprocessor);
        assert_eq!(gic.gicd().ITARGETSR[32].get(), 0x55);

        gic.route_interrupt_to_cpu(
            intid,
            CpuInterfaceTarget::Explicit(TargetList::from_raw(0x40)),
        );
        assert_eq!(gic.gicd().ITARGETSR[32].get(), 0x40);
    }
}
