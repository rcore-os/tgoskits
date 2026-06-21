use core::ptr::NonNull;

use aarch64_cpu::{
    asm::barrier,
    registers::{CurrentEL, MPIDR_EL1},
};
use log::*;
pub use tock_registers::{LocalRegisterCopy, interfaces::*};

mod gicd;
mod gicr;

use gicd::*;
use gicr::*;

use crate::version::{IrqVecReadable, IrqVecWriteable};
pub use crate::{IntId, VirtAddr, define::Trigger, sys_reg::*};

/// SGI target specification for GICv3.
///
/// Defines how to target CPUs when sending Software Generated Interrupts (SGIs).
/// Unlike GICv2, GICv3 uses affinity-based targeting through system registers.
#[derive(Debug, Clone, Copy)]
pub enum SGITarget {
    /// Send SGI to the current CPU (using IRM=1).
    All,
    /// Send SGI to specific CPUs identified by affinity and target list.
    List(TargetList),
}

impl SGITarget {
    /// Create a target for the current CPU.
    pub fn current() -> Self {
        let affinity = Affinity::current();
        Self::list([affinity]) // Only target current CPU
    }

    /// Create a target for specific CPUs using affinity routing.
    ///
    /// # Arguments
    ///
    /// * `affinity` - The base affinity (aff3, aff2, aff1)
    /// * `target_list` - Bitmap of target CPUs at affinity level 0
    pub fn list(list: impl AsRef<[Affinity]>) -> Self {
        Self::List(TargetList::new(list))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TargetList {
    /// Affinity level 3 (highest level)
    aff3: u8,
    /// Affinity level 2
    aff2: u8,
    /// Affinity level 1
    aff1: u8,
    /// Target list bitmap (16-bit) identifying CPUs at affinity level 0
    target_list: u16,
}

impl TargetList {
    /// Create a new TargetList with a specific CPU target list. list is Cpu interface IDs.
    pub fn new(list: impl AsRef<[Affinity]>) -> Self {
        let mut aff3 = 0;
        let mut aff2 = 0;
        let mut aff1 = 0;
        let mut raw = 0;
        for (i, aff) in list.as_ref().iter().enumerate() {
            if i == 0 {
                aff3 = aff.aff3;
                aff2 = aff.aff2;
                aff1 = aff.aff1;
            } else {
                assert!(
                    aff.aff3 == aff3 && aff.aff2 == aff2 && aff.aff1 == aff1,
                    "All targets must have the same affinity levels except for level 0"
                );
            }
            raw |= 1 << aff.aff0; // Set bit for each target CPU
        }
        Self {
            aff3,
            aff2,
            aff1,
            target_list: raw,
        }
    }

    pub fn add(&mut self, affinity: Affinity) {
        assert!(
            affinity.aff3 == self.aff3 && affinity.aff2 == self.aff2 && affinity.aff1 == self.aff1,
            "All targets must have the same affinity levels except for level 0"
        );
        self.target_list |= 1 << affinity.aff0; // Set bit for the target CPU
    }

    pub fn affinity_list(&self) -> impl Iterator<Item = Affinity> {
        (0..16)
            .filter(move |i| (self.target_list & (1 << i)) != 0)
            .map(move |i| Affinity {
                aff3: self.aff3,
                aff2: self.aff2,
                aff1: self.aff1,
                aff0: i as u8,
            })
    }
}

/// Affinity routing information for GICv3.
///
/// Represents the multi-level affinity routing used in GICv3 to identify
/// CPU cores in a hierarchical manner. This matches the MPIDR_EL1 register
/// format used by ARMv8 processors.
///
/// # Affinity Levels
///
/// - `aff0`: Level 0 affinity (typically core within cluster)
/// - `aff1`: Level 1 affinity (typically cluster within group)
/// - `aff2`: Level 2 affinity (typically group within system)
/// - `aff3`: Level 3 affinity (highest level, for large systems)
///
/// # Examples
///
/// ```
/// use arm_gic_driver::v3::Affinity;
///
/// // Create affinity for core 2 in cluster 1
/// let aff = Affinity {
///     aff0: 2, // Core 2
///     aff1: 1, // Cluster 1
///     aff2: 0, // Group 0
///     aff3: 0, // System 0
/// };
///
/// // Get current CPU's affinity
/// let current = Affinity::current();
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Affinity {
    /// Affinity level 0 (lowest level, typically core ID within cluster)
    pub aff0: u8,
    /// Affinity level 1 (typically cluster ID within group)
    pub aff1: u8,
    /// Affinity level 2 (typically group ID within system)
    pub aff2: u8,
    /// Affinity level 3 (highest level, for very large systems)
    pub aff3: u8,
}

impl Affinity {
    pub(crate) fn affinity(&self) -> u32 {
        self.aff0 as u32
            | ((self.aff1 as u32) << 8)
            | ((self.aff2 as u32) << 16)
            | ((self.aff3 as u32) << 24)
    }

    /// Create an `Affinity` from an MPIDR register value.
    ///
    /// Extracts the affinity levels from the Multiprocessor Affinity Register
    /// (MPIDR_EL1) which uniquely identifies each CPU core.
    ///
    /// # Arguments
    ///
    /// * `mpidr` - The MPIDR_EL1 register value
    ///
    /// # Returns
    ///
    /// An `Affinity` structure with the extracted affinity levels.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use aarch64_cpu::registers::MPIDR_EL1;
    /// use arm_gic_driver::v3::Affinity;
    ///
    /// let mpidr_value = MPIDR_EL1.get();
    /// let affinity = Affinity::from_mpidr(mpidr_value);
    /// ```
    pub fn from_mpidr(mpidr: u64) -> Self {
        let val = LocalRegisterCopy::<u64, MPIDR_EL1::Register>::new(mpidr);
        Self {
            aff0: val.read(MPIDR_EL1::Aff0) as u8,
            aff1: val.read(MPIDR_EL1::Aff1) as u8,
            aff2: val.read(MPIDR_EL1::Aff2) as u8,
            aff3: val.read(MPIDR_EL1::Aff3) as u8,
        }
    }

    /// Get the affinity of the current CPU core.
    ///
    /// Reads the MPIDR_EL1 register to determine the current CPU's affinity.
    /// This is commonly used to identify which CPU core is executing the code.
    ///
    /// # Returns
    ///
    /// An `Affinity` structure representing the current CPU's affinity.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use arm_gic_driver::v3::Affinity;
    ///
    /// let current_cpu = Affinity::current();
    /// println!(
    ///     "Running on CPU {}.{}.{}.{}",
    ///     current_cpu.aff3, current_cpu.aff2, current_cpu.aff1, current_cpu.aff0
    /// );
    /// ```
    pub fn current() -> Self {
        Self::from_mpidr(MPIDR_EL1.get())
    }
}

/// GICv3 driver implementation.
///
/// This structure provides the main interface for controlling a GICv3 interrupt controller.
/// It manages both the Distributor (GICD) for system-wide interrupt control and provides
/// access to Redistributors (GICR) for per-CPU interrupt management.
///
/// # Architecture
///
/// GICv3 consists of several components:
/// - **Distributor**: Controls SPIs (Shared Peripheral Interrupts) and global configuration
/// - **Redistributors**: Handle SGIs (Software Generated Interrupts) and PPIs (Private Peripheral Interrupts) for each CPU
/// - **CPU Interface**: System register-based interface for interrupt acknowledgment and EOI
///
/// # Security States
///
/// GICv3 supports different security configurations:
/// - **Single Security State**: All interrupts treated equally (DS=1)
/// - **Two Security States**: Separate Secure and Non-secure interrupt handling (DS=0)
///
/// # Examples
///
/// ```no_run
/// use arm_gic_driver::{VirtAddr, v3::Gic};
///
/// // Initialize GICv3 with memory-mapped register addresses
/// let gicd_addr = VirtAddr::new(0x0800_0000);
/// let gicr_addr = VirtAddr::new(0x0806_0000);
///
/// let mut gic = unsafe { Gic::new(gicd_addr, gicr_addr) };
/// gic.init();
///
/// // Initialize CPU interface for current CPU
/// let mut cpu_if = gic.cpu_interface();
/// cpu_if.init_current_cpu().unwrap();
/// ```
pub struct Gic {
    gicd: VirtAddr,
    gicr: VirtAddr,
    security_state: SecurityState,
}

unsafe impl Send for Gic {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct GicInitOptions {
    /// Reset interrupt distributor array registers during initialization.
    ///
    /// Keep this enabled for normal GICv3 hardware. Some emulated platforms may
    /// expose GICv3 but reject early GICD interrupt array accesses.
    pub reset_interrupts: bool,
}

impl Default for GicInitOptions {
    fn default() -> Self {
        Self {
            reset_interrupts: true,
        }
    }
}

impl Gic {
    /// Create a new GICv3 driver instance.
    ///
    /// # Arguments
    ///
    /// * `gicd` - Virtual address of the GIC Distributor register block
    /// * `gicr` - Virtual address of the GIC Redistributor register block
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The provided addresses point to valid, properly mapped GICv3 register blocks
    /// - The memory regions remain valid for the lifetime of the `Gic` instance
    /// - Only one `Gic` instance controls these hardware resources at a time
    /// - The addresses are correctly aligned according to GICv3 specification
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use arm_gic_driver::{VirtAddr, v3::Gic};
    ///
    /// let gicd_base = VirtAddr::new(0x0800_0000);
    /// let gicr_base = VirtAddr::new(0x0806_0000);
    ///
    /// let gic = unsafe { Gic::new(gicd_base, gicr_base) };
    /// ```
    pub const unsafe fn new(gicd: VirtAddr, gicr: VirtAddr) -> Self {
        Self {
            gicd,
            gicr,
            security_state: SecurityState::Single,
        }
    }

    fn gicd(&self) -> &DistributorReg {
        unsafe { &*self.gicd.as_ptr() }
    }

    pub fn gicr_addr(&self) -> VirtAddr {
        self.gicr
    }

    pub fn gicd_addr(&self) -> VirtAddr {
        self.gicd
    }
    /// Initialize the GICv3 Distributor according to ARM GIC Architecture Specification v3/v4
    ///
    /// This function implements the initialization sequence described in section 12.9.4
    /// of the ARM GIC Architecture Specification, supporting different security configurations:
    ///
    /// 1. **Single Security State**: When DS=1, only one security state exists
    ///    - Uses EnableGrp0 and EnableGrp1 bits
    ///    - Uses ARE bit for affinity routing
    ///
    /// 2. **Two Security States**: When DS=0, both Secure and Non-secure states exist
    ///    - Uses EnableGrp0, EnableGrp1NS, and EnableGrp1S bits
    ///    - Uses ARE_S and ARE_NS bits for separate affinity routing control
    ///
    /// The initialization sequence:
    /// 1. Disable all interrupt groups
    /// 2. Wait for register writes to complete (RWP=0)
    /// 3. Initialize distributor registers to known state
    /// 4. Configure CTLR based on security state
    /// 5. Enable affinity routing
    /// 6. Enable appropriate interrupt groups
    ///
    /// # Panics
    ///
    /// Panics if register write operations timeout, indicating hardware issues.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use arm_gic_driver::{VirtAddr, v3::Gic};
    ///
    /// let mut gic = unsafe { Gic::new(VirtAddr::new(0x0800_0000), VirtAddr::new(0x0806_0000)) };
    /// gic.init(); // Initialize the distributor
    /// ```
    pub fn init(&mut self) {
        self.init_with_options(GicInitOptions::default());
    }

    /// Initialize the GICv3 Distributor with platform-specific options.
    pub fn init_with_options(&mut self, options: GicInitOptions) {
        // Read current configuration to determine security state

        self.security_state = self.gicd().get_security_state();

        trace!(
            "Initializing GICv3 Distributor@{:#p}, security state: {:?}...",
            self.gicd.as_ptr::<u8>(),
            self.security_state
        );

        // 1. Disable all interrupt groups before configuration
        self.disable();
        barrier::isb(barrier::SY);

        // Wait for register write to complete
        if let Err(e) = self.gicd().wait_for_rwp() {
            panic!("Failed to disable GICv3 during init: {}", e);
        }
        trace!("GICv3 Distributor disabled");

        if options.reset_interrupts {
            self.gicd().reset_registers();
        }

        let ctrl = match self.security_state {
            SecurityState::Secure => {
                // In secure state, enable Group 1 Non-secure and Affinity Routing for Non-secure
                (CTLR_S::EnableGrp0::SET
                    + CTLR_S::EnableGrp1NS::SET
                    + CTLR_S::ARE_S::SET
                    + CTLR_S::ARE_NS::SET)
                    .value
            }
            SecurityState::NonSecure => {
                // In non-secure state, enable Group 1 and Affinity Routing
                (CTLR_NS::EnableGrp1::SET + CTLR_NS::EnableGrp1A::SET + CTLR_NS::ARE_NS::SET).value
            }
            SecurityState::Single => {
                // In single security state, enable both groups and Affinity Routing
                (CTLR_ONE::EnableGrp0::SET + CTLR_ONE::EnableGrp1::SET + CTLR_ONE::ARE::SET).value
            }
        };
        self.gicd().CTLR.set(ctrl);

        barrier::isb(barrier::SY);

        // Wait for final configuration to complete
        if let Err(e) = self.gicd().wait_for_rwp() {
            panic!("Failed to complete GICv3 initialization: {}", e);
        }
    }

    /// Get the maximum interrupt ID supported by this GIC implementation.
    ///
    /// Returns the highest interrupt ID that can be used with this GIC.
    /// This is determined by the GICD_TYPER.IDbits field which indicates
    /// the number of interrupt ID bits implemented.
    ///
    /// # Returns
    ///
    /// The maximum interrupt ID (typically 1019 for standard GICv3, or higher
    /// for implementations with extended interrupt ID support).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let max_id = gic.max_intid();
    /// println!("GIC supports interrupt IDs up to {}", max_id);
    /// ```
    pub fn max_intid(&self) -> u32 {
        self.gicd().max_intid()
    }

    fn disable(&self) {
        let old = self.gicd().CTLR.get();
        let val = match self.security_state {
            SecurityState::Secure => {
                (CTLR_S::EnableGrp0::CLEAR
                    + CTLR_S::EnableGrp1S::CLEAR
                    + CTLR_S::EnableGrp1NS::CLEAR)
                    .value
            }
            SecurityState::NonSecure => {
                (CTLR_NS::EnableGrp1::CLEAR + CTLR_NS::EnableGrp1A::CLEAR).value
            }
            SecurityState::Single => {
                (CTLR_ONE::EnableGrp0::CLEAR + CTLR_ONE::EnableGrp1::CLEAR).value
            }
        };
        self.gicd().CTLR.set(old & !val);
        barrier::isb(barrier::SY);
    }

    fn rd_slice(&self) -> RDv3Slice {
        RDv3Slice::new(unsafe { NonNull::new_unchecked(self.gicr.as_ptr()) })
    }

    fn current_rd_ref(&self) -> &RedistributorV3 {
        unsafe { self.current_rd().as_ref() }
    }

    fn current_rd(&self) -> NonNull<RedistributorV3> {
        let want = (MPIDR_EL1.get() & 0xFFFFFF) as u32;

        for rd in self.rd_slice().iter() {
            let affi = unsafe { rd.as_ref() }
                .lpi_ref()
                .TYPER
                .read(gicr::TYPER::Affinity) as u32;
            if affi == want {
                return rd;
            }
        }
        panic!("No current redistributor")
    }

    /// Get a CPU interface for the current CPU.
    ///
    /// Returns a `CpuInterface` that provides access to the current CPU's
    /// interrupt interface, including SGI/PPI control and interrupt
    /// acknowledgment/completion operations.
    ///
    /// # Returns
    ///
    /// A `CpuInterface` instance for the current CPU core.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let mut cpu_if = gic.cpu_interface();
    /// cpu_if.init_current_cpu().unwrap();
    /// ```
    pub fn cpu_interface(&self) -> CpuInterface {
        CpuInterface {
            rd: self.current_rd().as_ptr(),
            security_state: self.security_state,
        }
    }

    /// Enable or disable a shared peripheral interrupt (SPI).
    ///
    /// This function controls the enable state of SPIs through the distributor.
    /// Private interrupts (SGIs and PPIs) must be controlled through the
    /// CPU interface instead.
    ///
    /// # Arguments
    ///
    /// * `intid` - The interrupt ID to configure (must be an SPI)
    /// * `enable` - `true` to enable the interrupt, `false` to disable it
    ///
    /// # Panics
    ///
    /// Panics if `intid` represents a private interrupt (SGI or PPI).
    /// Use the CPU interface for private interrupt control.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{IntId, VirtAddr, v3::Gic};
    /// # let mut gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let spi = IntId::spi(42);
    /// gic.set_irq_enable(spi, true); // Enable SPI 42
    /// gic.set_irq_enable(spi, false); // Disable SPI 42
    /// ```
    pub fn set_irq_enable(&mut self, intid: IntId, enable: bool) {
        if intid.is_private() {
            self.current_rd_ref()
                .sgi
                .set_enable_interrupt(intid, enable);
        } else if enable {
            self.gicd().irq_enable(intid.to_u32());
        } else {
            self.gicd().irq_disable(intid.to_u32());
        }
    }

    /// Check if an interrupt is enabled.
    ///
    /// Returns the enable state of the specified interrupt.
    /// For SPIs, this checks the distributor registers.
    /// For private interrupts, use the CPU interface instead.
    ///
    /// # Arguments
    ///
    /// * `id` - The interrupt ID to check
    ///
    /// # Returns
    ///
    /// `true` if the interrupt is enabled, `false` otherwise.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{IntId, VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let spi = IntId::spi(42);
    /// if gic.is_irq_enable(spi) {
    ///     println!("SPI 42 is enabled");
    /// }
    /// ```
    pub fn is_irq_enable(&self, id: IntId) -> bool {
        if id.is_private() {
            self.current_rd_ref().sgi.is_interrupt_enabled(id)
        } else {
            self.gicd().ISENABLER.get_irq_bit(id.into())
        }
    }

    /// Set the priority of an interrupt.
    ///
    /// Sets the priority level for the specified interrupt. Lower values
    /// indicate higher priority. The actual number of priority bits
    /// implemented varies by GIC implementation.
    ///
    /// # Arguments
    ///
    /// * `intid` - The interrupt ID to configure
    /// * `priority` - Priority value (0 = highest, 255 = lowest)
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{IntId, VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let spi = IntId::spi(42);
    /// gic.set_priority(spi, 0x80); // Set to medium priority
    /// ```
    pub fn set_priority(&self, intid: IntId, priority: u8) {
        if intid.is_private() {
            self.current_rd_ref().sgi.set_priority(intid, priority);
        } else {
            self.gicd().set_priority(intid.to_u32(), priority);
        }
    }

    /// Get the priority of an interrupt.
    ///
    /// Returns the current priority level of the specified interrupt.
    ///
    /// # Arguments
    ///
    /// * `intid` - The interrupt ID to query
    ///
    /// # Returns
    ///
    /// The current priority value (0 = highest, 255 = lowest).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{IntId, VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let spi = IntId::spi(42);
    /// let priority = gic.get_priority(spi);
    /// println!("SPI 42 priority: {}", priority);
    /// ```
    pub fn get_priority(&self, intid: IntId) -> u8 {
        if intid.is_private() {
            self.current_rd_ref().sgi.get_priority(intid)
        } else {
            self.gicd().get_priority(intid.to_u32())
        }
    }

    /// Set the active state of an interrupt.
    ///
    /// Controls whether an interrupt is marked as active. An active interrupt
    /// is one that has been acknowledged but not yet completed (EOI sent).
    ///
    /// # Arguments
    ///
    /// * `id` - The interrupt ID to modify
    /// * `active` - `true` to mark as active, `false` to clear active state
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{IntId, VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let spi = IntId::spi(42);
    /// gic.set_active(spi, true); // Mark as active
    /// gic.set_active(spi, false); // Clear active state
    /// ```
    pub fn set_active(&self, id: IntId, active: bool) {
        if id.is_private() {
            self.current_rd_ref().sgi.set_active(id, active);
        } else if active {
            self.gicd().ISACTIVER.set_irq_bit(id.into());
        } else {
            self.gicd().ICACTIVER.set_irq_bit(id.into());
        }
    }

    /// Check if an interrupt is active.
    ///
    /// Returns whether the specified interrupt is currently in the active state.
    ///
    /// # Arguments
    ///
    /// * `id` - The interrupt ID to check
    ///
    /// # Returns
    ///
    /// `true` if the interrupt is active, `false` otherwise.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{IntId, VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let spi = IntId::spi(42);
    /// if gic.is_active(spi) {
    ///     println!("SPI 42 is active");
    /// }
    /// ```
    pub fn is_active(&self, id: IntId) -> bool {
        if id.is_private() {
            self.current_rd_ref().sgi.is_active(id)
        } else {
            self.gicd().ISACTIVER.get_irq_bit(id.into())
        }
    }

    /// Set the pending state of an interrupt.
    ///
    /// Controls whether an interrupt is marked as pending. A pending interrupt
    /// is one that has been signaled but not yet acknowledged.
    ///
    /// # Arguments
    ///
    /// * `id` - The interrupt ID to modify
    /// * `pending` - `true` to mark as pending, `false` to clear pending state
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{IntId, VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let spi = IntId::spi(42);
    /// gic.set_pending(spi, true); // Trigger interrupt
    /// gic.set_pending(spi, false); // Clear pending state
    /// ```
    pub fn set_pending(&self, id: IntId, pending: bool) {
        if id.is_private() {
            self.current_rd_ref().sgi.set_pending(id, pending);
        } else if pending {
            self.gicd().set_pending(id.into());
        } else {
            self.gicd().clear_pending(id.into());
        }
    }

    /// Check if an interrupt is pending.
    ///
    /// Returns whether the specified interrupt is currently pending.
    ///
    /// # Arguments
    ///
    /// * `id` - The interrupt ID to check
    ///
    /// # Returns
    ///
    /// `true` if the interrupt is pending, `false` otherwise.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{IntId, VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let spi = IntId::spi(42);
    /// if gic.is_pending(spi) {
    ///     println!("SPI 42 is pending");
    /// }
    /// ```
    pub fn is_pending(&self, id: IntId) -> bool {
        if id.is_private() {
            self.current_rd_ref().sgi.is_pending(id)
        } else {
            self.gicd().ISPENDR.get_irq_bit(id.into())
        }
    }

    /// Get the raw IIDR (Implementer Identification Register) value.
    ///
    /// Returns the raw GICD_IIDR register value which contains
    /// implementation-specific identification information.
    ///
    /// # Returns
    ///
    /// The raw IIDR register value containing implementer ID, revision,
    /// variant, and product ID fields.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let iidr = gic.iidr_raw();
    /// println!("GIC implementer ID: {:#x}", iidr);
    /// ```
    pub fn iidr_raw(&self) -> u32 {
        self.gicd().IIDR.get()
    }

    /// Get the raw TYPER (Type Register) value.
    ///
    /// Returns the raw GICD_TYPER register value which contains
    /// information about the GIC configuration and capabilities.
    ///
    /// # Returns
    ///
    /// The raw TYPER register value containing information about
    /// interrupt lines, CPU interfaces, security extensions, etc.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let typer = gic.typer_raw();
    /// let it_lines = (typer & 0x1f) + 1;
    /// println!("GIC supports {} interrupt lines", it_lines * 32);
    /// ```
    pub fn typer_raw(&self) -> u32 {
        self.gicd().TYPER.get()
    }

    /// Set the trigger type configuration for an interrupt.
    ///
    /// Configures whether an interrupt is triggered by signal edges or levels.
    /// This affects how the GIC samples and processes the interrupt signal.
    ///
    /// # Arguments
    ///
    /// * `id` - The interrupt ID to configure
    /// * `cfg` - The trigger type (`Edge` or `Level`)
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use arm_gic_driver::{IntId, Trigger, VirtAddr, v3::Gic};
    /// # let gic = unsafe { Gic::new(VirtAddr::new(0), VirtAddr::new(0)) };
    /// let spi = IntId::spi(42);
    /// gic.set_cfg(spi, Trigger::Edge); // Configure as edge-triggered
    /// gic.set_cfg(spi, Trigger::Level); // Configure as level-triggered
    /// ```
    pub fn set_cfg(&self, id: IntId, cfg: Trigger) {
        if id.is_private() {
            // Apply to all redistributors since private interrupts are per-CPU
            for rd in self.rd_slice().iter() {
                unsafe { rd.as_ref() }.sgi.set_cfgr(id, cfg);
            }
        } else {
            self.gicd().set_interrupt_config(id, cfg);
        }
    }

    pub fn get_cfg(&self, id: IntId) -> Trigger {
        if id.is_private() {
            self.current_rd_ref().sgi.get_cfgr(id)
        } else {
            let int_num = id.to_u32();
            let reg_index = (int_num / 16) as usize;
            let bit_offset = (int_num % 16) * 2 + 1; // Each interrupt uses 2 bits, we use bit 1 for edge/level

            assert!(
                reg_index < self.gicd().ICFGR.len(),
                "Invalid interrupt ID for config: {id:?}"
            );

            let current = self.gicd().ICFGR[reg_index].get();
            let mask = 1 << bit_offset;

            if current & mask != 0 {
                Trigger::Edge
            } else {
                Trigger::Level
            }
        }
    }

    /// If `affinity` is `None`, interrupts routed to any PE defined as a participating node.
    pub fn set_target_cpu(&self, id: IntId, affinity: Option<Affinity>) {
        // Only SPIs (Shared Peripheral Interrupts) can have their target CPU set
        // SGIs and PPIs are always private to a specific CPU core
        assert!(
            !id.is_private(),
            "Cannot set target CPU for private interrupt (SGI/PPI): {id:?}"
        );
        self.gicd().set_interrupt_route(id.to_u32(), affinity);
    }

    pub fn get_target_cpu(&self, id: IntId) -> Option<Affinity> {
        // Only SPIs (Shared Peripheral Interrupts) can have their target CPU set
        // SGIs and PPIs are always private to a specific CPU core
        assert!(
            !id.is_private(),
            "Cannot get target CPU for private interrupt (SGI/PPI): {id:?}"
        );
        self.gicd().get_interrupt_route(id.to_u32())
    }

    pub fn max_cpu_num(&self) -> usize {
        self.gicd().max_cpu_num() as _
    }
}

/// Every CPU interface has its own GICC registers
pub struct CpuInterface {
    rd: *mut RedistributorV3,
    security_state: SecurityState,
}

unsafe impl Send for CpuInterface {}

impl CpuInterface {
    fn rd(&self) -> &RedistributorV3 {
        unsafe { &*self.rd }
    }

    /// Initialize the CPU interface for the current CPU
    ///
    /// This follows the GICv3 architecture specification for CPU interface initialization:
    /// 1. Wake up the Redistributor
    /// 2. Initialize SGI/PPI registers to known state
    /// 3. Configure CPU interface registers
    pub fn init_current_cpu(&mut self) -> Result<(), &'static str> {
        let cpu = Affinity::current();
        trace!(
            "CPU interface initialization for CPU: {:#x}",
            cpu.affinity()
        );

        // 1. Wake up the Redistributor first
        self.rd().lpi.wake()?;

        // 2. Initialize SGI/PPI registers with proper sequence
        self.rd().sgi.init_sgi_ppi(self.security_state);

        // Wait for register writes to complete
        self.rd().lpi.wait_for_rwp()?;

        // 3. Configure CPU interface system registers
        if CurrentEL.read(CurrentEL::EL) == 2 {
            ICC_SRE_EL2.write(
                ICC_SRE_EL2::SRE::SET
                    + ICC_SRE_EL2::DFB::SET
                    + ICC_SRE_EL2::DIB::SET
                    + ICC_SRE_EL2::ENABLE::SET,
            );
        } else {
            ICC_SRE_EL1
                .write(ICC_SRE_EL1::SRE::SET + ICC_SRE_EL1::DFB::SET + ICC_SRE_EL1::DIB::SET);
        }

        // 4. Set interrupt priority mask to allow all priorities (using 8-bit priority)
        ICC_PMR_EL1.write(ICC_PMR_EL1::PRIORITY.val(0xFF));

        // 5. Enable appropriate interrupt groups based on security state
        match self.security_state {
            SecurityState::Single => {
                // In single security state, enable both Group 0 and Group 1
                ICC_IGRPEN0_EL1.write(ICC_IGRPEN0_EL1::ENABLE::SET);
                ICC_IGRPEN1_EL1.write(ICC_IGRPEN1_EL1::ENABLE::SET);
                // Use common binary point register
                ICC_CTLR_EL1.modify(ICC_CTLR_EL1::CBPR::SET);
            }
            SecurityState::Secure => {
                // In secure state, enable both groups
                ICC_IGRPEN0_EL1.write(ICC_IGRPEN0_EL1::ENABLE::SET);
                ICC_IGRPEN1_EL1.write(ICC_IGRPEN1_EL1::ENABLE::SET);
            }
            SecurityState::NonSecure => {
                // In non-secure state, only enable Group 1
                ICC_IGRPEN1_EL1.write(ICC_IGRPEN1_EL1::ENABLE::SET);
                ICC_CTLR_EL1.modify(ICC_CTLR_EL1::CBPR::SET);
            }
        }

        // 6. Configure EOI mode
        if CurrentEL.read(CurrentEL::EL) == 2 {
            ICC_CTLR_EL1.modify(ICC_CTLR_EL1::EOIMODE::SET);
        }

        trace!("CPU interface initialized successfully");
        Ok(())
    }

    /// Set the EOI mode for non-secure interrupts
    ///
    /// - `false` GICC_EOIR has both priority drop and deactivate interrupt functionality. Accesses to the GICC_DIR are UNPREDICTABLE.
    /// - `true`  GICC_EOIR has priority drop functionality only. GICC_DIR has deactivate interrupt functionality.
    pub fn set_eoi_mode(&self, is_two_step: bool) {
        ICC_CTLR_EL1.modify(if is_two_step {
            ICC_CTLR_EL1::EOIMODE::SET
        } else {
            ICC_CTLR_EL1::EOIMODE::CLEAR
        });
    }

    pub fn eoi_mode(&self) -> bool {
        ICC_CTLR_EL1.is_set(ICC_CTLR_EL1::EOIMODE)
    }

    pub fn ack0(&self) -> IntId {
        let raw = ICC_IAR0_EL1.read(ICC_IAR0_EL1::INTID) as u32;
        unsafe { IntId::raw(raw) }
    }

    pub fn ack1(&self) -> IntId {
        let raw = ICC_IAR1_EL1.read(ICC_IAR1_EL1::INTID) as u32;
        unsafe { IntId::raw(raw) }
    }

    pub fn eoi0(&self, ack: IntId) {
        ICC_EOIR0_EL1.write(ICC_EOIR0_EL1::INTID.val(ack.to_u32() as _));
    }

    pub fn eoi1(&self, ack: IntId) {
        ICC_EOIR1_EL1.write(ICC_EOIR1_EL1::INTID.val(ack.to_u32() as _));
    }

    /// Deactivate an interrupt
    pub fn dir(&self, ack: IntId) {
        ICC_DIR_EL1.write(ICC_DIR_EL1::INTID.val(ack.to_u32() as _));
    }

    /// Set the priority mask (interrupts with priority >= mask will be masked)
    pub fn set_priority_mask(&self, mask: u8) {
        ICC_PMR_EL1.write(ICC_PMR_EL1::PRIORITY.val(mask as _));
    }

    pub fn set_irq_enable(&self, id: IntId, enable: bool) {
        assert!(
            id.is_private(),
            "Cannot enable non-private interrupt: {id:?}"
        );
        self.rd().sgi.set_enable_interrupt(id, enable);
    }

    pub fn is_irq_enable(&self, id: IntId) -> bool {
        assert!(
            id.is_private(),
            "Cannot check non-private interrupt: {id:?}"
        );
        self.rd().sgi.is_interrupt_enabled(id)
    }

    /// Set interrupt priority (0 = highest priority, 255 = lowest priority)
    pub fn set_priority(&self, id: IntId, priority: u8) {
        assert!(
            id.is_private(),
            "Cannot set priority for non-private interrupt: {id:?}"
        );

        self.rd().sgi.set_priority(id, priority);
    }

    pub fn get_priority(&self, id: IntId) -> u8 {
        assert!(
            id.is_private(),
            "Cannot get priority for non-private interrupt: {id:?}"
        );
        self.rd().sgi.get_priority(id)
    }

    pub fn set_active(&self, id: IntId, active: bool) {
        assert!(
            id.is_private(),
            "Cannot set active state for non-private interrupt: {id:?}"
        );
        self.rd().sgi.set_active(id, active);
    }

    pub fn is_active(&self, id: IntId) -> bool {
        assert!(
            id.is_private(),
            "Cannot check active state for non-private interrupt: {id:?}"
        );
        self.rd().sgi.is_active(id)
    }

    pub fn set_pending(&self, id: IntId, pending: bool) {
        assert!(
            id.is_private(),
            "Cannot set pending state for non-private interrupt: {id:?}"
        );
        self.rd().sgi.set_pending(id, pending);
    }

    pub fn is_pending(&self, id: IntId) -> bool {
        assert!(
            id.is_private(),
            "Cannot check pending state for non-private interrupt: {id:?}"
        );
        self.rd().sgi.is_pending(id)
    }

    pub fn set_cfg(&self, id: IntId, cfg: Trigger) {
        assert!(
            id.is_private(),
            "Cannot set config for non-private interrupt: {id:?}"
        );
        self.rd().sgi.set_cfgr(id, cfg);
    }

    pub fn get_cfg(&self, id: IntId) -> Trigger {
        assert!(
            id.is_private(),
            "Cannot get config for non-private interrupt: {id:?}"
        );
        self.rd().sgi.get_cfgr(id)
    }

    pub fn send_sgi(&self, sgi_id: IntId, target: SGITarget) {
        send_sgi(sgi_id, target);
    }

    pub const fn trap_operations(&self) -> TrapOp {
        TrapOp {}
    }
}

pub struct TrapOp {}

unsafe impl Send for TrapOp {}
unsafe impl Sync for TrapOp {}

impl TrapOp {
    pub fn eoi_mode(&self) -> bool {
        eoi_mode()
    }

    pub fn ack0(&self) -> IntId {
        ack0()
    }

    pub fn ack1(&self) -> IntId {
        ack1()
    }

    pub fn eoi0(&self, ack: IntId) {
        eoi0(ack);
    }

    pub fn eoi1(&self, ack: IntId) {
        eoi1(ack);
    }

    /// Deactivate an interrupt
    pub fn dir(&self, ack: IntId) {
        dir(ack);
    }
}

pub fn eoi_mode() -> bool {
    ICC_CTLR_EL1.is_set(ICC_CTLR_EL1::EOIMODE)
}

pub fn ack0() -> IntId {
    let raw = ICC_IAR0_EL1.read(ICC_IAR0_EL1::INTID) as u32;
    unsafe { IntId::raw(raw) }
}

pub fn ack1() -> IntId {
    let raw = ICC_IAR1_EL1.read(ICC_IAR1_EL1::INTID) as u32;
    unsafe { IntId::raw(raw) }
}

pub fn eoi0(ack: IntId) {
    ICC_EOIR0_EL1.write(ICC_EOIR0_EL1::INTID.val(ack.to_u32() as _));
}

pub fn eoi1(ack: IntId) {
    ICC_EOIR1_EL1.write(ICC_EOIR1_EL1::INTID.val(ack.to_u32() as _));
}

/// Deactivate an interrupt
pub fn dir(ack: IntId) {
    ICC_DIR_EL1.write(ICC_DIR_EL1::INTID.val(ack.to_u32() as _));
}

/// Send a Software Generated Interrupt (SGI) to target CPUs.
///
/// In GICv3, SGIs are sent using system registers ICC_SGI1R_EL1 and ICC_SGI0_EL1
/// instead of the legacy GICD_SGIR register used in GICv2.
///
/// # Arguments
///
/// * `sgi_id` - SGI interrupt ID (0-15)
/// * `target` - Target specification for the SGI
///
/// # Example
///
/// ```ignore
/// use arm_gic_driver::IntId;
/// use arm_gic_driver::v3::SGITarget;
///
/// // Send SGI 5 to all other CPUs
/// let sgi_id = IntId::sgi(5);
/// arm_gic_driver::v3::send_sgi(sgi_id, SGITarget::AllOther);
/// ```
pub fn send_sgi(sgi_id: IntId, target: SGITarget) {
    assert!(sgi_id.is_sgi(), "Invalid SGI ID: {sgi_id:?}");

    let sgi_num = sgi_id.to_u32();

    match target {
        SGITarget::All => {
            trace!("Sending SGI {sgi_num} to all CPUs");
            ICC_SGI1R_EL1.write(ICC_SGI1R_EL1::INTID.val(sgi_num as u64) + ICC_SGI1R_EL1::IRM::SET);
        }
        SGITarget::List(val) => {
            trace!("Sending SGI {sgi_num} to CPUs with affinity: {val:#x?}");
            // Send to specific CPUs identified by affinity and target list
            let value = ICC_SGI1R_EL1::INTID.val(sgi_num as u64)
                + ICC_SGI1R_EL1::AFF3.val(val.aff3 as u64)
                + ICC_SGI1R_EL1::AFF2.val(val.aff2 as u64)
                + ICC_SGI1R_EL1::AFF1.val(val.aff1 as u64)
                + ICC_SGI1R_EL1::TARGETLIST.val(val.target_list as u64);
            ICC_SGI1R_EL1.write(value);
        }
    }
    barrier::isb(barrier::SY);
}
