use tock_registers::{register_bitfields, register_structs, registers::*};

register_structs! {
    /// GIC CPU Interface registers.
    #[allow(non_snake_case)]
    pub CpuInterfaceReg {
        /// CPU Interface Control Register.
        (0x0000 => pub CTLR: ReadWrite<u32, CTLR::Register>),
        /// Interrupt Priority Mask Register.
        (0x0004 => pub PMR: ReadWrite<u32, PMR::Register>),
        /// Binary Point Register.
        (0x0008 => pub BPR: ReadWrite<u32, BPR::Register>),
        /// Interrupt Acknowledge Register.
        (0x000c => pub IAR: ReadOnly<u32, IAR::Register>),
        /// End of Interrupt Register.
        (0x0010 => pub EOIR: WriteOnly<u32, EOIR::Register>),
        /// Running Priority Register.
        (0x0014 => pub RPR: ReadOnly<u32, RPR::Register>),
        /// Highest Priority Pending Interrupt Register.
        (0x0018 => pub HPPIR: ReadOnly<u32, HPPIR::Register>),
        /// Aliased Binary Point Register.
        (0x001c => pub ABPR: ReadWrite<u32, ABPR::Register>),
        /// Aliased Interrupt Acknowledge Register.
        (0x0020 => pub AIAR: ReadOnly<u32, AIAR::Register>),
        /// Aliased End of Interrupt Register.
        (0x0024 => pub AEOIR: WriteOnly<u32, AEOIR::Register>),
        /// Aliased Highest Priority Pending Interrupt Register.
        (0x0028 => pub AHPPIR: ReadOnly<u32, AHPPIR::Register>),
        (0x002c => _reserved_1),
        /// Active Priorities Registers.
        (0x00d0 => pub APR: [ReadWrite<u32>; 4]),
        /// Non-secure Active Priorities Registers.
        (0x00e0 => pub NSAPR: [ReadWrite<u32>; 4]),
        (0x00f0 => _reserved_2),
        /// CPU Interface Identification Register.
        (0x00fc => pub IIDR: ReadOnly<u32>),
        (0x0100 => _reserved_3),
        /// Deactivate Interrupt Register.
        (0x1000 => pub DIR: WriteOnly<u32, DIR::Register>),
        (0x1004 => @END),
    }
}

register_bitfields! [
    u32,
    /// CPU Interface Control Register
    pub CTLR [
        /// Enable Group 0 interrupts
        EnableGrp0 OFFSET(0) NUMBITS(1) [],
        /// Enable Group 1 interrupts
        EnableGrp1 OFFSET(1) NUMBITS(1) [],
        /// Acknowledge control for Group 1 interrupts
        AckCtl OFFSET(2) NUMBITS(1) [],
        /// FIQ enable for Group 0 interrupts
        FIQEn OFFSET(3) NUMBITS(1) [],
        /// Common binary point register
        CBPR OFFSET(4) NUMBITS(1) [],
        /// FIQ bypass disable for Group 0
        FIQBypDisGrp0 OFFSET(5) NUMBITS(1) [],
        /// IRQ bypass disable for Group 0
        IRQBypDisGrp0 OFFSET(6) NUMBITS(1) [],
        /// FIQ bypass disable for Group 1
        FIQBypDisGrp1 OFFSET(7) NUMBITS(1) [],
        /// IRQ bypass disable for Group 1
        IRQBypDisGrp1 OFFSET(8) NUMBITS(1) [],
        /// EOI mode for Non-secure state
        EOImodeNS OFFSET(9) NUMBITS(1) [],
    ],

    /// Interrupt Acknowledge Register
    pub IAR [
        /// Interrupt ID
        InterruptID OFFSET(0) NUMBITS(10) [],
        /// CPU ID (for SGIs)
        CPUID OFFSET(10) NUMBITS(3) [],
    ],

    /// Priority Mask Register
    pub PMR [
        /// Priority
        Priority OFFSET(0) NUMBITS(8) [],
    ],

    /// Binary Point Register
    pub BPR [
        /// Binary point
        BinaryPoint OFFSET(0) NUMBITS(3) [],
    ],

    /// Running Priority Register
    pub RPR [
        /// Priority
        Priority OFFSET(0) NUMBITS(8) [],
    ],

    /// Highest Priority Pending Interrupt Register
    pub HPPIR [
        /// Pending interrupt ID
        PENDINTID OFFSET(0) NUMBITS(10) [],
        /// CPU ID (for SGIs)
        CPUID OFFSET(10) NUMBITS(3) [],
    ],

    /// Aliased Binary Point Register
    pub ABPR [
        /// Binary point
        BinaryPoint OFFSET(0) NUMBITS(3) [],
    ],

    /// Aliased Interrupt Acknowledge Register
    pub AIAR [
        /// Interrupt ID
        InterruptID OFFSET(0) NUMBITS(10) [],
        /// CPU ID (for SGIs)
        CPUID OFFSET(10) NUMBITS(3) [],
    ],

    /// Aliased End of Interrupt Register
    pub AEOIR [
        /// End of interrupt ID
        EOIINTID OFFSET(0) NUMBITS(10) [],
        /// CPU ID (for SGIs)
        CPUID OFFSET(10) NUMBITS(3) [],
    ],

    /// Aliased Highest Priority Pending Interrupt Register
    pub AHPPIR [
        /// Pending interrupt ID
        PENDINTID OFFSET(0) NUMBITS(10) [],
        /// CPU ID (for SGIs)
        CPUID OFFSET(10) NUMBITS(3) [],
    ],

    /// End of Interrupt Register
    pub EOIR [
        /// End of interrupt ID
        EOIINTID OFFSET(0) NUMBITS(10) [],
        /// CPU ID (for SGIs)
        CPUID OFFSET(10) NUMBITS(3) [],
    ],

    /// Deactivate Interrupt Register
    pub DIR [
        /// Interrupt ID
        InterruptID OFFSET(0) NUMBITS(10) [],
        /// CPU ID (for SGIs)
        CPUID OFFSET(10) NUMBITS(3) [],
    ],
];
