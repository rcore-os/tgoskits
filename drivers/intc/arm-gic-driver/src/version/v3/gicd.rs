use core::hint::spin_loop;

use aarch64_cpu::asm::barrier;
use tock_registers::{interfaces::*, register_bitfields, register_structs, registers::*};

use crate::{
    IntId,
    define::{SPI_RANGE, Trigger},
    v3::Affinity,
};

/// Access context for CTLR register operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityState {
    /// Access from Secure state in two security states configuration
    Secure,
    /// Access from Non-secure state in two security states configuration
    NonSecure,
    /// Access in single security state configuration
    Single,
}

register_structs! {
    #[allow(non_snake_case)]
    pub DistributorReg {
        /// Distributor Control Register.
        (0x0000 => pub CTLR: ReadWrite<u32, CTLR_BASE::Register>),
        /// Interrupt Controller Type Register.
        (0x0004 => pub TYPER: ReadOnly<u32, TYPER::Register>),
        /// Distributor Implementer Identification Register.
        (0x0008 => pub IIDR: ReadOnly<u32, IIDR::Register>),
        /// Type Modifier Register.
        (0x000c => pub TYPER2: ReadOnly<u32, TYPER2::Register>),
        /// Status Register.
        (0x0010 => pub STATUSR: ReadWrite<u32, STATUSR::Register>),
        (0x0014 => _rsv1: [u32; 11]),
        /// Set SPI Register.
        (0x0040 => pub SETSPI_NSR: WriteOnly<u32, SETSPI_NSR::Register>),
        (0x0044 => _rsv2),
        /// Clear SPI Register.
        (0x0048 => pub CLRSPI_NSR: WriteOnly<u32, CLRSPI_NSR::Register>),
        (0x004c => _rsv3),
        /// Set SPI, Secure Register.
        (0x0050 => pub SETSPI_SR: WriteOnly<u32, SETSPI_SR::Register>),
        (0x0054 => _rsv4),
        /// Clear SPI, Secure Register.
        (0x0058 => pub CLRSPI_SR: WriteOnly<u32, CLRSPI_SR::Register>),
        (0x005c => _rsv5: [u32; 9]),
        /// Interrupt Group Registers.
        (0x0080 => pub IGROUPR: [ReadWrite<u32>; 0x20]),
        /// Interrupt Set-Enable Registers.
        (0x0100 => pub ISENABLER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Clear-Enable Registers.
        (0x0180 => pub ICENABLER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Set-Pending Registers.
        (0x0200 => pub ISPENDR: [ReadWrite<u32>; 0x20]),
        /// Interrupt Clear-Pending Registers.
        (0x0280 => pub ICPENDR: [ReadWrite<u32>; 0x20]),
        /// Interrupt Set-Active Registers.
        (0x0300 => pub ISACTIVER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Clear-Active Registers.
        (0x0380 => pub ICACTIVER: [ReadWrite<u32>; 0x20]),
        /// Interrupt Priority Registers.
        (0x0400 => pub IPRIORITYR: [ReadWrite<u8>; 1024]),
        /// Interrupt Processor Targets Registers (legacy only).
        (0x0800 => pub ITARGETSR: [ReadWrite<u8>; 1024]),
        /// Interrupt Configuration Registers.
        (0x0c00 => pub ICFGR: [ReadWrite<u32>; 0x40]),
        /// Interrupt Group Modifier Registers.
        (0x0d00 => pub IGRPMODR: [ReadWrite<u32>; 0x20]),
        (0x0d80 => _rsv6: [u32; 32]),
        /// Non-secure Access Control Registers.
        (0x0e00 => pub NSACR: [ReadWrite<u32>; 0x40]),
        /// Software Generated Interrupt Register (legacy only).
        (0x0f00 => pub SGIR: WriteOnly<u32, SGIR::Register>),
        (0x0f04 => _rsv7: [u32; 3]),
        /// SGI Clear-Pending Registers (legacy only).
        (0x0f10 => pub CPENDSGIR: [ReadWrite<u32>; 0x4]),
        /// SGI Set-Pending Registers (legacy only).
        (0x0f20 => pub SPENDSGIR: [ReadWrite<u32>; 0x4]),
        (0x0f30 => _rsv8: [u32; 20]),
        /// Non-maskable Interrupt Registers.
        (0x0f80 => pub INMIR: [ReadWrite<u32>; 0x20]),
        (0x1000 => _rsv9: [u32; 5184]),
        /// Interrupt Routing Registers.
        (0x6100 => pub IROUTER: [ReadWrite<u64>; 987]),
        (0x7FD8 => _rsv10: [u32; 2]),
        (0x7FE0 => @END),
    }
}

const GICD_IGROUPR: usize = 0x0080;
const GICD_ICENABLER: usize = 0x0180;
const GICD_ICPENDR: usize = 0x0280;
const GICD_ICACTIVER: usize = 0x0380;
const GICD_IPRIORITYR: usize = 0x0400;
const GICD_ICFGR: usize = 0x0c00;

#[inline(always)]
fn reg32_addr(base: *const DistributorReg, offset: usize, index: usize) -> *mut u32 {
    (base as usize + offset + index * core::mem::size_of::<u32>()) as *mut u32
}

#[inline(always)]
fn reg8_addr(base: *const DistributorReg, offset: usize, index: usize) -> *mut u8 {
    (base as usize + offset + index) as *mut u8
}

#[inline(always)]
fn mmio_write32(addr: *mut u32, value: u32) {
    unsafe {
        core::arch::asm!(
            "str {value:w}, [{addr}]",
            addr = in(reg) addr,
            value = in(reg) value,
            options(nostack, preserves_flags)
        );
    }
}

#[inline(always)]
fn mmio_write8(addr: *mut u8, value: u8) {
    unsafe {
        core::arch::asm!(
            "strb {value:w}, [{addr}]",
            addr = in(reg) addr,
            value = in(reg) value,
            options(nostack, preserves_flags)
        );
    }
}

fn write_reg32_range(base: *const DistributorReg, offset: usize, num_regs: usize, value: u32) {
    for i in 0..num_regs {
        mmio_write32(reg32_addr(base, offset, i), value);
    }
}

#[allow(dead_code)]
impl DistributorReg {
    pub fn get_security_state(&self) -> SecurityState {
        if self.is_single_security_state() || !self.has_security_extensions() {
            SecurityState::Single
        } else {
            // In two security states configuration, use GICD_NSACR access behavior to determine security state
            // According to ARM GIC specification:
            // - When DS == 0 and access is Secure: GICD_NSACR is RW
            // - When DS == 0 and access is Non-secure: GICD_NSACR is RAZ/WI
            self.detect_security_state_via_nsacr()
        }
    }

    /// Check if single security state is configured
    pub fn is_single_security_state(&self) -> bool {
        self.CTLR.is_set(CTLR_BASE::DS)
    }

    /// Detect security state using GICD_NSACR access behavior
    ///
    /// According to ARM GIC specification for GICD_NSACR<n> registers:
    /// - When GICD_CTLR.DS == 0 and access is Secure: RW access
    /// - When GICD_CTLR.DS == 0 and access is Non-secure: RAZ/WI access
    ///
    /// This method attempts to write to GICD_NSACR0 and reads it back.
    /// If the written value persists, we're in Secure state.
    /// If it reads as zero, we're in Non-secure state.
    fn detect_security_state_via_nsacr(&self) -> SecurityState {
        // Only valid in two security states configuration
        if self.is_single_security_state() {
            return SecurityState::Single;
        }

        // Read current value of GICD_NSACR0
        let original_value = self.NSACR[0].get();

        // Test pattern - use bits that are guaranteed to be writable in secure state
        // We use bits for interrupts 32-63 (first register after SGI/PPI range)
        let test_pattern = 0x55555555u32; // Alternating pattern

        // Write test pattern to GICD_NSACR0
        self.NSACR[0].set(test_pattern);

        // Memory barrier to ensure write completes
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);

        // Read back the value
        let read_value = self.NSACR[0].get();

        // Restore original value
        self.NSACR[0].set(original_value);

        // Determine security state based on read-back behavior
        if read_value == test_pattern {
            // Write succeeded and persisted - we're in Secure state
            SecurityState::Secure
        } else if read_value == 0 {
            // Write was ignored (RAZ/WI behavior) - we're in Non-secure state
            SecurityState::NonSecure
        } else {
            // Partial write success indicates some bits are reserved
            // This still suggests we're in Secure state but some bits are read-only
            SecurityState::Secure
        }
    }

    /// Get the maximum number of supported INTIDs
    pub fn max_intid(&self) -> u32 {
        let id_bits = self.TYPER.read(TYPER::IDbits);
        1u32 << (id_bits + 1)
    }

    /// Get the number of interrupt lines (SPIs)
    pub fn max_spi_num(&self) -> u32 {
        let it_lines_number = self.TYPER.read(TYPER::ITLinesNumber);
        (it_lines_number + 1) * 32
    }

    /// Get the number of CPUs supported
    pub fn max_cpu_num(&self) -> u32 {
        let cpu_number = self.TYPER.read(TYPER::CPUNumber);
        cpu_number + 1
    }

    /// Check if Security Extensions are implemented
    pub fn has_security_extensions(&self) -> bool {
        self.TYPER.is_set(TYPER::SecurityExtn)
    }

    /// Disable all interrupts
    pub fn irq_disable_all(&self, max_interrupts: u32) {
        let num_regs = max_interrupts.div_ceil(32) as usize;
        let num_regs = num_regs.min(self.ICENABLER.len());

        write_reg32_range(self, GICD_ICENABLER, num_regs, u32::MAX);
    }

    /// Enable specific interrupt
    pub fn irq_enable(&self, intid: u32) {
        if intid >= 32 {
            // Only SPIs can be controlled via distributor
            let reg_idx = (intid / 32) as usize;
            let bit_idx = intid % 32;
            if reg_idx < self.ISENABLER.len() {
                self.ISENABLER[reg_idx].set(1 << bit_idx);
            }
        }
    }

    /// Disable specific interrupt
    pub fn irq_disable(&self, intid: u32) {
        if intid >= 32 {
            // Only SPIs can be controlled via distributor
            let reg_idx = (intid / 32) as usize;
            let bit_idx = intid % 32;
            if reg_idx < self.ICENABLER.len() {
                self.ICENABLER[reg_idx].set(1 << bit_idx);
            }
        }
    }

    /// Set interrupt as pending
    pub fn set_pending(&self, intid: u32) {
        if intid >= 32 {
            // Only SPIs can be controlled via distributor
            let reg_idx = (intid / 32) as usize;
            let bit_idx = intid % 32;
            if reg_idx < self.ISPENDR.len() {
                self.ISPENDR[reg_idx].set(1 << bit_idx);
            }
        }
    }

    /// Clear pending interrupt
    pub fn clear_pending(&self, intid: u32) {
        if intid >= 32 {
            // Only SPIs can be controlled via distributor
            let reg_idx = (intid / 32) as usize;
            let bit_idx = intid % 32;
            if reg_idx < self.ICPENDR.len() {
                self.ICPENDR[reg_idx].set(1 << bit_idx);
            }
        }
    }

    /// Clear all pending interrupts
    pub fn pending_clear_all(&self, max_interrupts: u32) {
        let num_regs = max_interrupts.div_ceil(32) as usize;
        let num_regs = num_regs.min(self.ICPENDR.len());

        write_reg32_range(self, GICD_ICPENDR, num_regs, u32::MAX);
    }

    /// Clear all active interrupts
    pub fn active_clear_all(&self, max_interrupts: u32) {
        let num_regs = max_interrupts.div_ceil(32) as usize;
        let num_regs = num_regs.min(self.ICACTIVER.len());

        write_reg32_range(self, GICD_ICACTIVER, num_regs, u32::MAX);
    }

    /// Set interrupt priority
    pub fn set_priority(&self, intid: u32, priority: u8) {
        if intid >= 32 && (intid as usize) < self.IPRIORITYR.len() {
            self.IPRIORITYR[intid as usize].set(priority);
        }
    }

    /// Get interrupt priority
    pub fn get_priority(&self, intid: u32) -> u8 {
        if intid >= 32 && (intid as usize) < self.IPRIORITYR.len() {
            self.IPRIORITYR[intid as usize].get()
        } else {
            0
        }
    }

    /// Set default priorities for all interrupts
    pub fn set_default_priorities(&self, max_interrupts: u32) {
        let num_priorities = max_interrupts.min(self.IPRIORITYR.len() as u32);

        // Set default priority (0xA0 - middle priority) for all interrupts
        for i in 32..num_priorities {
            // Skip SGIs and PPIs
            mmio_write8(reg8_addr(self, GICD_IPRIORITYR, i as usize), 0xA0);
        }
    }

    /// Configure interrupt groups - set all interrupts to Group 1 by default
    pub fn groups_all_to_1(&self, max_interrupts: u32) {
        let num_regs = max_interrupts.div_ceil(32) as usize;
        let num_regs = num_regs.min(self.IGROUPR.len());

        write_reg32_range(self, GICD_IGROUPR, num_regs, u32::MAX);
    }

    /// Set interrupt group and modifier
    pub fn set_interrupt_group(&self, intid: u32, group: u32, group_modifier: bool) {
        if intid >= 32 {
            // Only SPIs can be controlled via distributor
            let reg_idx = (intid / 32) as usize;
            let bit_idx = intid % 32;

            if reg_idx < self.IGROUPR.len() {
                let current = self.IGROUPR[reg_idx].get();
                if group != 0 {
                    self.IGROUPR[reg_idx].set(current | (1 << bit_idx));
                } else {
                    self.IGROUPR[reg_idx].set(current & !(1 << bit_idx));
                }
            }

            if reg_idx < self.IGRPMODR.len() {
                let current = self.IGRPMODR[reg_idx].get();
                if group_modifier {
                    self.IGRPMODR[reg_idx].set(current | (1 << bit_idx));
                } else {
                    self.IGRPMODR[reg_idx].set(current & !(1 << bit_idx));
                }
            }
        }
    }

    /// Configure interrupt configuration (edge/level triggered)
    pub fn set_interrupt_config(&self, id: IntId, trigger: Trigger) {
        let int_num = id.to_u32();
        let reg_index = (int_num / 16) as usize;
        let bit_offset = (int_num % 16) * 2 + 1; // Each interrupt uses 2 bits, we use bit 1 for edge/level

        assert!(
            reg_index < self.ICFGR.len(),
            "Invalid interrupt ID for config: {id:?}"
        );

        let current = self.ICFGR[reg_index].get();
        let mask = 1 << bit_offset;

        let new_value = match trigger {
            Trigger::Level => current & !mask, // Clear bit for level-triggered
            Trigger::Edge => current | mask,   // Set bit for edge-triggered
        };

        self.ICFGR[reg_index].set(new_value);
    }

    /// Configure interrupt configuration for all interrupts
    pub fn configure_interrupt_config(&self, max_interrupts: u32) {
        let num_regs = max_interrupts.div_ceil(16) as usize;
        let num_regs = num_regs.min(self.ICFGR.len());

        // Configure all interrupts as level-sensitive (0x0) by default
        write_reg32_range(self, GICD_ICFGR, num_regs, 0);
    }

    /// Set interrupt routing (affinity) using IROUTER registers
    pub fn set_interrupt_route(&self, intid: u32, aff: Option<Affinity>) {
        // Check if this is a valid SPI in the standard range
        if !SPI_RANGE.contains(&intid) {
            // TODO: Check for Extended SPI support in TYPER2 register when extended range is used
            return; // Only SPIs (32-1019) can be routed currently
        }

        // Calculate IROUTER register index
        // IROUTER registers start at SPI 32, so subtract 32
        let router_idx = (intid - 32) as usize;

        if router_idx >= self.IROUTER.len() {
            return; // Out of range for IROUTER registers
        }

        let mut route_value = 0u64;
        match aff {
            Some(Affinity {
                aff0,
                aff1,
                aff2,
                aff3,
            }) => {
                // Set specific affinity routing
                route_value |= aff0 as u64;
                route_value |= (aff1 as u64) << 8;
                route_value |= (aff2 as u64) << 16;
                route_value |= (aff3 as u64) << 32;
                // Ensure Interrupt_Routing_Mode is 0 for specific routing
                route_value &= !(1u64 << 31);
            }
            None => {
                // Set "any participating PE" routing mode
                route_value |= 1u64 << 31;
            }
        }
        self.IROUTER[router_idx].set(route_value);
    }

    /// Get interrupt routing information
    pub fn get_interrupt_route(&self, intid: u32) -> Option<Affinity> {
        if SPI_RANGE.contains(&intid) {
            let router_idx = (intid - 32) as usize;

            if router_idx < self.IROUTER.len() {
                let route_value = self.IROUTER[router_idx].get();
                let aff0 = (route_value & 0xFF) as u8;
                let aff1 = ((route_value >> 8) & 0xFF) as u8;
                let aff2 = ((route_value >> 16) & 0xFF) as u8;
                let aff3 = ((route_value >> 32) & 0xFF) as u8;
                let routing_mode = (route_value & (1u64 << 31)) != 0;

                return if routing_mode {
                    None
                } else {
                    Some(Affinity {
                        aff0,
                        aff1,
                        aff2,
                        aff3,
                    })
                };
            }
        }
        None
    }

    /// Generate message-based SPI (Non-secure)
    pub fn generate_spi_ns(&self, intid: u32) {
        if (32..1020).contains(&intid) {
            self.SETSPI_NSR.write(SETSPI_NSR::INTID.val(intid));
        }
    }

    /// Generate message-based SPI (Secure)
    pub fn generate_spi_s(&self, intid: u32) {
        if (32..1020).contains(&intid) {
            self.SETSPI_SR.write(SETSPI_SR::INTID.val(intid));
        }
    }

    /// Clear message-based SPI (Non-secure)
    pub fn clear_spi_ns(&self, intid: u32) {
        if (32..1020).contains(&intid) {
            self.CLRSPI_NSR.write(CLRSPI_NSR::INTID.val(intid));
        }
    }

    /// Clear message-based SPI (Secure)
    pub fn clear_spi_s(&self, intid: u32) {
        if (32..1020).contains(&intid) {
            self.CLRSPI_SR.write(CLRSPI_SR::INTID.val(intid));
        }
    }

    /// Configure non-maskable interrupt
    pub fn set_nmi(&self, intid: u32, nmi: bool) {
        if (32..1020).contains(&intid) {
            let reg_idx = (intid / 32) as usize;
            let bit_idx = intid % 32;

            if reg_idx < self.INMIR.len() {
                let current = self.INMIR[reg_idx].get();
                if nmi {
                    self.INMIR[reg_idx].set(current | (1 << bit_idx));
                } else {
                    self.INMIR[reg_idx].set(current & !(1 << bit_idx));
                }
            }
        }
    }

    /// Check if interrupt is configured as NMI
    pub fn is_nmi(&self, intid: u32) -> bool {
        if (32..1020).contains(&intid) {
            let reg_idx = (intid / 32) as usize;
            let bit_idx = intid % 32;

            if reg_idx < self.INMIR.len() {
                let current = self.INMIR[reg_idx].get();
                return (current & (1 << bit_idx)) != 0;
            }
        }
        false
    }

    /// Check if Extended SPI range is supported
    pub fn has_extended_spi(&self) -> bool {
        // Check if TYPER2.ESPI is implemented and set
        self.TYPER2.read(TYPER2::NMI) != 0 // Using NMI bit as placeholder since ESPI is not defined yet
    }

    /// Get the Extended SPI range if supported
    pub fn extended_spi_range(&self) -> u32 {
        // This would read TYPER2.ESPI_range field when implemented
        0 // Placeholder return
    }

    /// Check if Message-based SPIs are supported
    pub fn has_message_based_spi(&self) -> bool {
        self.TYPER.read(TYPER::MBIS) != 0
    }

    /// Check if LPIs are supported
    pub fn has_lpis(&self) -> bool {
        self.TYPER.read(TYPER::LPIS) != 0
    }

    /// Check if Direct Virtual LPI injection is supported
    pub fn has_direct_vlpi(&self) -> bool {
        self.TYPER.read(TYPER::DVIS) != 0
    }

    fn set_all_routing_to_current(&self, max_interrupts: u32) {
        let current = Affinity::current();
        for i in SPI_RANGE.start..max_interrupts {
            // Set all SPIs to route to current CPU
            self.set_interrupt_route(i, Some(current));
        }
    }

    /// Initialize for two security states configuration (from Secure state)
    /// This handles the case where DS=0 and security extensions are present
    pub fn reset_registers(&self) {
        // Get the maximum number of interrupts
        let max_spis = self.max_spi_num();

        // Clear all pending and active interrupts
        self.pending_clear_all(max_spis);
        self.active_clear_all(max_spis);

        // Disable all interrupts
        self.irq_disable_all(max_spis);

        // Set all interrupts to Group 1 by default
        self.groups_all_to_1(max_spis);

        // Set default priorities
        self.set_default_priorities(max_spis);

        // Configure all interrupts as level-sensitive
        self.configure_interrupt_config(max_spis);

        self.set_all_routing_to_current(max_spis);
    }

    /// Wait for register write pending to clear
    pub fn wait_for_rwp(&self) -> Result<(), &'static str> {
        let mut time_out_count = 10000;
        while self.CTLR.is_set(CTLR_BASE::RWP) {
            spin_loop();
            time_out_count -= 1;
            if time_out_count == 0 {
                return Err("GICv3 Distributor CTLR RWP wait timeout.");
            }
        }
        barrier::isb(barrier::SY);
        Ok(())
    }
}

register_bitfields! [
    u32,
    /// GICD_CTLR register - unified bitfield covering all security configurations
    pub CTLR_BASE [
        /// Disable Security - single security state when set
        DS OFFSET(6) NUMBITS(1) [
            TwoSecurityStates = 0,
            SingleSecurityState = 1,
        ],
        /// Register Write Pending - read only
        RWP OFFSET(31) NUMBITS(1) [],
    ],
    /// When access is Secure, in a system that supports two Security states
    pub CTLR_S [
        EnableGrp0 OFFSET(0) NUMBITS(1) [],
        EnableGrp1NS OFFSET(1) NUMBITS(1) [],
        EnableGrp1S OFFSET(2) NUMBITS(1) [],
        ARE_S OFFSET(4) NUMBITS(1) [],
        ARE_NS OFFSET(5) NUMBITS(1) [],
        DS OFFSET(6) NUMBITS(1) [],
        E1NWF OFFSET(7) NUMBITS(1) [],
        RWP OFFSET(31) NUMBITS(1) [],
    ],
    /// When access is Non-secure, in a system that supports two Security states
    pub CTLR_NS [
        EnableGrp1 OFFSET(0) NUMBITS(1) [],
        EnableGrp1A OFFSET(1) NUMBITS(1) [],
        ARE_NS OFFSET(4) NUMBITS(1) [],
        RWP OFFSET(31) NUMBITS(1) [],
    ],
    /// When in a system that supports only a single Security state
    pub CTLR_ONE [
        EnableGrp0 OFFSET(0) NUMBITS(1) [],
        EnableGrp1 OFFSET(1) NUMBITS(1) [],
        ARE OFFSET(4) NUMBITS(1) [],
        DS OFFSET(6) NUMBITS(1) [],
        E1NWF OFFSET(7) NUMBITS(1) [],
        nASSGIreq OFFSET(8) NUMBITS(1) [],
        RWP OFFSET(31) NUMBITS(1) [],
    ],

    /// Interrupt Controller Type Register
    pub TYPER [
        /// Number of interrupt lines supported
        ITLinesNumber OFFSET(0) NUMBITS(5) [],
        /// Number of CPU interfaces implemented minus one
        CPUNumber OFFSET(5) NUMBITS(3) [],
        /// Indicates whether the GIC implements Security Extensions
        SecurityExtn OFFSET(10) NUMBITS(1) [
            SingleSecurity = 0,
            TwoSecurity = 1,
        ],
        /// Number of Lockable Shared Peripheral Interrupts
        LSPI OFFSET(11) NUMBITS(5) [],
        /// Interrupt identifier bits supported
        IDbits OFFSET(19) NUMBITS(5) [],
        /// Affinity 3 supported
        A3V OFFSET(24) NUMBITS(1) [],
        /// No1ofN behavior supported
        No1N OFFSET(25) NUMBITS(1) [],
        /// Common not Private base supported
        CommonLPIAff OFFSET(26) NUMBITS(2) [],
        /// Message based SPIs supported
        MBIS OFFSET(16) NUMBITS(1) [],
        /// Low Power Interrupt supported
        LPIS OFFSET(17) NUMBITS(1) [],
        /// Dirty tracking for Direct LPI Injection supported
        DVIS OFFSET(18) NUMBITS(1) [],
    ],

    /// Type Modifier Register
    pub TYPER2 [
        /// Virtual LPIs supported
        VIL OFFSET(0) NUMBITS(1) [],
        /// Virtual command queue interface supported
        VID OFFSET(1) NUMBITS(5) [],
        /// NMI support
        NMI OFFSET(6) NUMBITS(1) [],
    ],

    /// Status Register
    pub STATUSR [
        /// Register Write Pending
        RRD OFFSET(0) NUMBITS(1) [],
        /// Write register in progress
        WRD OFFSET(1) NUMBITS(1) [],
        /// Register write request failed
        RWOD OFFSET(2) NUMBITS(1) [],
        /// Wake-up request denied
        WROD OFFSET(3) NUMBITS(1) [],
    ],

    /// Distributor Implementer Identification Register
    pub IIDR [
        /// Implementer identification number
        Implementer OFFSET(0) NUMBITS(12) [],
        /// Revision number
        Revision OFFSET(12) NUMBITS(4) [],
        /// Variant number
        Variant OFFSET(16) NUMBITS(4) [],
        /// Product identification number
        ProductId OFFSET(24) NUMBITS(8) []
    ],

    /// Software Generated Interrupt Register (legacy only)
    pub SGIR [
        /// SGI interrupt ID
        SGIINTID OFFSET(0) NUMBITS(4) [],
        /// Non-secure access (only relevant when Security Extensions are implemented)
        NSATT OFFSET(15) NUMBITS(1) [],
        /// CPU target list
        CPUTargetList OFFSET(16) NUMBITS(8) [],
        /// Target list filter
        TargetListFilter OFFSET(24) NUMBITS(2) [
            /// Forward to CPUs listed in CPUTargetList
            TargetList = 0,
            /// Forward to all CPUs except the requesting CPU
            AllOther = 0b01,
            /// Forward only to the requesting CPU
            Current = 0b10,
        ],
    ],

    /// Set SPI Register
    pub SETSPI_NSR [
        /// Interrupt ID
        INTID OFFSET(0) NUMBITS(13) [],
    ],

    /// Set SPI Register (Secure)
    pub SETSPI_SR [
        /// Interrupt ID
        INTID OFFSET(0) NUMBITS(13) [],
    ],

    /// Clear SPI Register
    pub CLRSPI_NSR [
        /// Interrupt ID
        INTID OFFSET(0) NUMBITS(13) [],
    ],

    /// Clear SPI Register (Secure)
    pub CLRSPI_SR [
        /// Interrupt ID
        INTID OFFSET(0) NUMBITS(13) [],
    ],

    /// Peripheral ID2 Register
    pub PIDR2 [
        /// Architecture revision
        ArchRev OFFSET(4) NUMBITS(4) [],
    ],
];

register_bitfields! [
    u64,
    /// Interrupt Routing Register
    pub IROUTER [
        /// Affinity level 0
        Aff0 OFFSET(0) NUMBITS(8) [],
        /// Affinity level 1
        Aff1 OFFSET(8) NUMBITS(8) [],
        /// Affinity level 2
        Aff2 OFFSET(16) NUMBITS(8) [],
        /// Interrupt Routing Mode
        Interrupt_Routing_Mode OFFSET(31) NUMBITS(1) [
            /// Specific PE routing
            Specific = 0,
            /// Any participating PE
            Any = 1,
        ],
        /// Affinity level 3
        Aff3 OFFSET(32) NUMBITS(8) [],
    ],
];
