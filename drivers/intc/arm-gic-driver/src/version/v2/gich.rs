use tock_registers::{register_bitfields, register_structs, registers::*};

register_structs! {
    /// GIC Hypervisor Interface Control registers.
    #[allow(non_snake_case)]
    pub HypervisorRegs {
        /// Hypervisor Control Register
        (0x000 => pub HCR: ReadWrite<u32, HCR::Register>),
        /// VGIC Type Register
        (0x004 => pub VTR: ReadOnly<u32, VTR::Register>),
        /// Virtual Machine Control Register
        (0x008 => pub VMCR: ReadWrite<u32, VMCR::Register>),
        (0x00c => _reserved_1),
        /// Maintenance Interrupt Status Register
        (0x010 => pub MISR: ReadOnly<u32, MISR::Register>),
        (0x014 => _reserved_2),
        /// End of Interrupt Status Registers
        (0x020 => pub EISR0: ReadOnly<u32>),
        (0x024 => pub EISR1: ReadOnly<u32>),
        (0x028 => _reserved_3),
        /// Empty List Register Status Registers
        (0x030 => pub ELRSR0: ReadOnly<u32>),
        (0x034 => pub ELRSR1: ReadOnly<u32>),
        (0x038 => _reserved_4),
        /// Active Priorities Register
        (0x0f0 => pub APR: ReadWrite<u32>),
        (0x0f4 => _reserved_5),
        /// List Registers
        (0x100 => pub LR: [ReadWrite<u32, LR::Register>; 64]),
        (0x200 => @END),
    }
}

register_bitfields! [
    u32,
    /// Hypervisor Control Register
    pub HCR [
        /// Global enable bit for the virtual CPU interface
        En OFFSET(0) NUMBITS(1) [],
        /// Underflow Interrupt Enable
        UIE OFFSET(1) NUMBITS(1) [],
        /// List Register Entry Not Present Interrupt Enable
        LRENPIE OFFSET(2) NUMBITS(1) [],
        /// No Pending Interrupt Enable
        NPIE OFFSET(3) NUMBITS(1) [],
        /// VM Enable Group 0 Interrupt Enable
        VGrp0EIE OFFSET(4) NUMBITS(1) [],
        /// VM Disable Group 0 Interrupt Enable
        VGrp0DIE OFFSET(5) NUMBITS(1) [],
        /// VM Enable Group 1 Interrupt Enable
        VGrp1EIE OFFSET(6) NUMBITS(1) [],
        /// VM Disable Group 1 Interrupt Enable
        VGrp1DIE OFFSET(7) NUMBITS(1) [],
        /// EOI Count
        EOICount OFFSET(27) NUMBITS(5) [],
    ],

    /// VGIC Type Register
    pub VTR [
        /// Number of implemented List registers minus one
        ListRegs OFFSET(0) NUMBITS(6) [],
        /// Number of preemption bits implemented minus one
        PREbits OFFSET(26) NUMBITS(3) [],
        /// Number of priority bits implemented minus one
        PRIbits OFFSET(29) NUMBITS(3) [],
    ],

    /// Virtual Machine Control Register
    pub VMCR [
        /// VM Group 0 Enable
        VMGrp0En OFFSET(0) NUMBITS(1) [],
        /// VM Group 1 Enable
        VMGrp1En OFFSET(1) NUMBITS(1) [],
        /// VM Acknowledge Control
        VMAckCtl OFFSET(2) NUMBITS(1) [],
        /// VM FIQ Enable
        VMFIQEn OFFSET(3) NUMBITS(1) [],
        /// VM Common Binary Point Register
        VMCBPR OFFSET(4) NUMBITS(1) [],
        /// VM EOI Mode
        VEM OFFSET(9) NUMBITS(1) [],
        /// VM Aliased Binary Point
        VMABP OFFSET(18) NUMBITS(3) [],
        /// VM Binary Point
        VMBP OFFSET(21) NUMBITS(3) [],
        /// VM Priority Mask
        VMPriMask OFFSET(27) NUMBITS(5) [],
    ],

    /// Maintenance Interrupt Status Register
    pub MISR [
        /// EOI maintenance interrupt
        EOI OFFSET(0) NUMBITS(1) [],
        /// Underflow maintenance interrupt
        U OFFSET(1) NUMBITS(1) [],
        /// List Register Entry Not Present maintenance interrupt
        LRENP OFFSET(2) NUMBITS(1) [],
        /// No Pending maintenance interrupt
        NP OFFSET(3) NUMBITS(1) [],
        /// Enabled Group 0 maintenance interrupt
        VGrp0E OFFSET(4) NUMBITS(1) [],
        /// Disabled Group 0 maintenance interrupt
        VGrp0D OFFSET(5) NUMBITS(1) [],
        /// Enabled Group 1 maintenance interrupt
        VGrp1E OFFSET(6) NUMBITS(1) [],
        /// Disabled Group 1 maintenance interrupt
        VGrp1D OFFSET(7) NUMBITS(1) [],
    ],

    /// List Register
    pub LR [
        /// Virtual ID
        VirtualID OFFSET(0) NUMBITS(10) [],
        /// Physical ID / CPU ID / EOI
        PhysicalID OFFSET(10) NUMBITS(10) [],
        /// CPU ID (when HW=0)
        CPUID OFFSET(10) NUMBITS(3) [],
        /// EOI (when HW=0)
        EOI OFFSET(19) NUMBITS(1) [],
        /// Priority
        Priority OFFSET(23) NUMBITS(5) [],
        /// State
        State OFFSET(28) NUMBITS(2) [
            Invalid = 0b00,
            Pending = 0b01,
            Active = 0b10,
            PendingAndActive = 0b11,
        ],
        /// Group 1
        Grp1 OFFSET(30) NUMBITS(1) [],
        /// Hardware interrupt
        HW OFFSET(31) NUMBITS(1) [],
    ],
];
