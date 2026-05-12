use tock_registers::{interfaces::*, register_bitfields, register_structs, registers::*};

use crate::{IntId, define::Trigger};

register_structs! {
    #[allow(non_snake_case)]
    pub DistributorReg {
        /// Distributor Control Register.
        (0x0000 => pub CTLR: ReadWrite<u32, CTLR::Register>),
        /// Interrupt Controller Type Register.
        (0x0004 => pub TYPER: ReadOnly<u32, TYPER::Register>),
        /// Distributor Implementer Identification Register.
        (0x0008 => pub IIDR: ReadOnly<u32, IIDR::Register>),
        (0x000c => _rsv1),
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
        /// Interrupt Processor Targets Registers.
        (0x0800 => pub ITARGETSR: [ReadWrite<u8>; 1024]),
        /// Interrupt Configuration Registers.
        (0x0c00 => pub ICFGR: [ReadWrite<u32>; 0x40]),
        /// Private Peripheral Interrupt Status Register.
        (0x0d00 => pub PPISR: ReadOnly<u32>),
        /// Shared Peripheral Interrupt Status Registers.
        (0x0d04 => pub SPISR: [ReadOnly<u32>; 0x1f]),
        (0x0d80 => _rsv2),
        /// Non-secure Access Control Registers.
        (0x0e00 => pub NSACR: [ReadWrite<u32>; 0x40]),
        /// Software Generated Interrupt Register.
        (0x0f00 => pub SGIR: WriteOnly<u32, SGIR::Register>),
        (0x0f04 => _rsv4),
        /// SGI Clear-Pending Registers.
        (0x0f10 => pub CPENDSGIR: [ReadWrite<u32>; 0x4]),
        /// SGI Set-Pending Registers.
        (0x0f20 => pub SPENDSGIR: [ReadWrite<u32>; 0x4]),
        (0x0f30 => _rsv5),
        /// Peripheral ID4 Register.
        (0x0fd0 => pub PIDR4: ReadOnly<u32>),
        /// Peripheral ID5 Register.
        (0x0fd4 => pub PIDR5: ReadOnly<u32>),
        /// Peripheral ID6 Register.
        (0x0fd8 => pub PIDR6: ReadOnly<u32>),
        /// Peripheral ID7 Register.
        (0x0fdc => pub PIDR7: ReadOnly<u32>),
        /// Peripheral ID0 Register.
        (0x0fe0 => pub PIDR0: ReadOnly<u32>),
        /// Peripheral ID1 Register.
        (0x0fe4 => pub PIDR1: ReadOnly<u32>),
        /// Peripheral ID2 Register.
        (0x0fe8 => pub PIDR2: ReadOnly<u32, PIDR2::Register>),
        /// Peripheral ID3 Register.
        (0x0fec => pub PIDR3: ReadOnly<u32>),
        /// Component ID0 Register.
        (0x0ff0 => pub CIDR0: ReadOnly<u32>),
        /// Component ID1 Register.
        (0x0ff4 => pub CIDR1: ReadOnly<u32>),
        /// Component ID2 Register.
        (0x0ff8 => pub CIDR2: ReadOnly<u32>),
        /// Component ID3 Register.
        (0x0ffc => pub CIDR3: ReadOnly<u32>),
        (0x1000 => @END),
    }
}

impl DistributorReg {
    /// Disable the GIC Distributor
    pub fn disable(&self) {
        self.CTLR
            .modify(CTLR::EnableGrp0::CLEAR + CTLR::EnableGrp1::CLEAR);
    }

    /// Enable the GIC Distributor for both Group 0 and Group 1 interrupts
    pub fn enable(&self) {
        self.CTLR
            .modify(CTLR::EnableGrp0::SET + CTLR::EnableGrp1::SET);
    }

    /// Disable all interrupts
    pub fn irq_disable_all(&self, max_interrupts: u32) {
        // Calculate number of ICENABLER registers needed
        let num_regs = max_interrupts.div_ceil(32) as usize;
        let num_regs = num_regs.min(self.ICENABLER.len());

        for i in 0..num_regs {
            self.ICENABLER[i].set(u32::MAX);
        }
    }

    /// Clear all pending interrupts
    pub(crate) fn pending_clear_all(&self, max_interrupts: u32) {
        // Calculate number of ICPENDR registers needed
        let num_regs = max_interrupts.div_ceil(32) as usize;
        let num_regs = num_regs.min(self.ICPENDR.len());

        for i in 0..num_regs {
            self.ICPENDR[i].set(u32::MAX);
        }
    }

    /// Clear all active interrupts
    pub(crate) fn active_clear_all(&self, max_interrupts: u32) {
        // Calculate number of ICACTIVER registers needed
        let num_regs = max_interrupts.div_ceil(32) as usize;
        let num_regs = num_regs.min(self.ICACTIVER.len());

        for i in 0..num_regs {
            self.ICACTIVER[i].set(u32::MAX);
        }
    }

    /// Configure interrupt groups - set all interrupts to Group 0 by default
    pub(crate) fn groups_all_to_0(&self, max_interrupts: u32) {
        // Calculate number of IGROUPR registers needed
        let num_regs = max_interrupts.div_ceil(32) as usize;
        let num_regs = num_regs.min(self.IGROUPR.len());

        for i in 0..num_regs {
            self.IGROUPR[i].set(0);
        }
    }

    /// Set default priorities for SGI and PPI (ID 0..31)
    pub(crate) fn set_default_sgi_ppi_priorities(&self) {
        // SGI and PPI: 32 interrupts
        let num_interrupts = 32;

        for i in 0..num_interrupts {
            self.IPRIORITYR[i].set(0xA0);
        }
    }

    /// Set default priorities for SPI (ID 32..max_interrupts-1)
    pub(crate) fn set_default_spi_priorities(&self, max_interrupts: u32) {
        let total_regs = max_interrupts.div_ceil(4) as usize;
        let total_regs = total_regs.min(self.IPRIORITYR.len());

        // SPI starts from interrupt ID 32
        let spi_start_id = 32;

        for i in spi_start_id..total_regs {
            self.IPRIORITYR[i].set(0xA0);
        }
    }

    /// Configure interrupt targets for SPIs (Shared Peripheral Interrupts)
    pub(crate) fn configure_interrupt_targets(&self, max_interrupts: u32) {
        // SGIs (0-15) and PPIs (16-31) don't use ITARGETSR
        // Only SPIs (32+) need target configuration
        if max_interrupts <= 32 {
            return;
        }

        let spi_start = 32;
        let num_spis = max_interrupts - spi_start;
        let num_regs = num_spis.div_ceil(4) as usize;
        let target_reg_start = (spi_start / 4) as usize;
        let target_reg_end = target_reg_start + num_regs;
        let target_reg_end = target_reg_end.min(self.ITARGETSR.len());

        // Set all SPIs to target CPU 0 by default (0x01)
        for i in target_reg_start..target_reg_end {
            self.ITARGETSR[i].set(0x01);
        }
    }

    /// Configure interrupt configuration (edge/level triggered)
    pub(crate) fn configure_interrupt_config(&self, max_interrupts: u32) {
        // Calculate number of ICFGR registers needed (16 interrupts per register)
        let num_regs = max_interrupts.div_ceil(16) as usize;
        let num_regs = num_regs.min(self.ICFGR.len());

        // Configure all interrupts as level-sensitive (0x0) by default
        // SGIs are always edge-triggered, but we can set the bits anyway
        for i in 0..num_regs {
            self.ICFGR[i].set(0);
        }
    }

    pub fn max_spi_num(&self) -> u32 {
        let it_lines_number = self.TYPER.read(TYPER::ITLinesNumber); // ITLinesNumber field
        (it_lines_number + 1) * 32
    }

    pub fn set_cfg(&self, id: IntId, cfg: Trigger) {
        let int_num = id.to_u32();
        let reg_index = (int_num / 16) as usize;
        let bit_offset = (int_num % 16) * 2 + 1; // Each interrupt uses 2 bits, we use bit 1 for edge/level

        assert!(
            reg_index < self.ICFGR.len(),
            "Invalid interrupt ID for config: {id:?}"
        );

        let current = self.ICFGR[reg_index].get();
        let mask = 1 << bit_offset;

        let new_value = match cfg {
            Trigger::Level => current & !mask, // Clear bit for level-triggered
            Trigger::Edge => current | mask,   // Set bit for edge-triggered
        };

        self.ICFGR[reg_index].set(new_value);
    }

    pub fn get_cfg(&self, id: IntId) -> Trigger {
        let int_num = id.to_u32();
        let reg_index = (int_num / 16) as usize;
        let bit_offset = (int_num % 16) * 2 + 1; // Each interrupt uses 2 bits, we use bit 1 for edge/level

        assert!(
            reg_index < self.ICFGR.len(),
            "Invalid interrupt ID for config: {id:?}"
        );

        let current = self.ICFGR[reg_index].get();
        let mask = 1 << bit_offset;

        if current & mask != 0 {
            Trigger::Edge
        } else {
            Trigger::Level
        }
    }
}

register_bitfields! [
    u32,
    /// Distributor Control Register (GICv2)
    pub CTLR [
        /// Enable Group 0 interrupts
        EnableGrp0 OFFSET(0) NUMBITS(1) [],
        /// Enable Group 1 interrupts
        EnableGrp1 OFFSET(1) NUMBITS(1) [],
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

    /// Software Generated Interrupt Register
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

    /// Peripheral ID2 Register
    pub PIDR2 [
        /// Architecture revision
        ArchRev OFFSET(4) NUMBITS(4) [],
    ],
];
