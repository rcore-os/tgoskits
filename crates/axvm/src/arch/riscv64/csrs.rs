use defs::*;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::RegisterLongName;

/// Define each registers of hypervisor using.
pub struct CSR {
    pub sie: ReadWriteCsr<sie::Register, CSR_SIE>,
    pub hstatus: ReadWriteCsr<hstatus::Register, CSR_HSTATUS>,
    pub hedeleg: ReadWriteCsr<hedeleg::Register, CSR_HEDELEG>,
    pub hideleg: ReadWriteCsr<hideleg::Register, CSR_HIDELEG>,
    pub hcounteren: ReadWriteCsr<hcounteren::Register, CSR_HCOUNTEREN>,
    pub hvip: ReadWriteCsr<hvip::Register, CSR_HVIP>,
}

#[allow(clippy::identity_op, clippy::erasing_op)]
pub const CSR: &CSR = &CSR {
    sie: ReadWriteCsr::new(),
    hstatus: ReadWriteCsr::new(),
    hedeleg: ReadWriteCsr::new(),
    hideleg: ReadWriteCsr::new(),
    hcounteren: ReadWriteCsr::new(),
    hvip: ReadWriteCsr::new(),
};

/// Trait defining the possible operations on a RISC-V CSR.
pub trait RiscvCsrTrait {
    type R: RegisterLongName;
    /// Reads the value of the CSR.
    fn get_value(&self) -> usize;

    /// Writes the value of the CSR.
    fn write_value(&self, value: usize);

    /// Atomicllt swaps the value of CSRs.
    fn atomic_replace(&self, value: usize) -> usize;

    /// Atomically read a CSR and set bits specified in a bitmask
    fn read_and_set_bits(&self, bitmasks: usize) -> usize;

    /// Atomically read a CSR and set bits specified in a bitmask
    fn read_and_clear_bits(&self, bitmasks: usize) -> usize;
}

/// Read/Write register.
pub struct ReadWriteCsr<R: RegisterLongName, const V: u16> {
    associated_register: core::marker::PhantomData<R>,
}

impl<R: RegisterLongName, const V: u16> ReadWriteCsr<R, V> {
    pub const fn new() -> Self {
        Self {
            associated_register: core::marker::PhantomData,
        }
    }
}

impl<R: RegisterLongName, const V: u16> RiscvCsrTrait for ReadWriteCsr<R, V> {
    type R = R;

    fn get_value(&self) -> usize {
        let r: usize;
        unsafe {
            core::arch::asm!("csrr {rd}, {csr}", rd = out(reg) r, csr = const V);
        }
        r
    }

    fn write_value(&self, value: usize) {
        unsafe {
            core::arch::asm!("csrw {csr}, {rs}", csr = const V, rs = in(reg) value);
        }
    }

    fn atomic_replace(&self, value: usize) -> usize {
        let r: usize;
        unsafe {
            core::arch::asm!("csrrw {rd}, {csr}, {rs}", rd = out(reg) r, csr = const V, rs = in(reg) value);
        }
        r
    }

    fn read_and_set_bits(&self, bitmask: usize) -> usize {
        let r: usize;
        unsafe {
            core::arch::asm!("csrrs {rd}, {csr}, {rs}", rd = out(reg) r, csr = const V, rs = in(reg) bitmask);
        }
        r
    }

    fn read_and_clear_bits(&self, bitmask: usize) -> usize {
        let r: usize;
        unsafe {
            core::arch::asm!("csrrc {rd}, {csr}, {rs}", rd = out(reg) r, csr = const V, rs = in(reg) bitmask);
        }
        r
    }
}

// The Readable and Writeable traits aren't object-safe so unfortunately we can't implement them
// for RiscvCsrInterface.
impl<R: RegisterLongName, const V: u16> Readable for ReadWriteCsr<R, V> {
    type T = usize;
    type R = R;

    fn get(&self) -> usize {
        self.get_value()
    }
}

impl<R: RegisterLongName, const V: u16> Writeable for ReadWriteCsr<R, V> {
    type T = usize;
    type R = R;

    fn set(&self, val_to_set: usize) {
        self.write_value(val_to_set);
    }
}

pub mod defs {
    use tock_registers::register_bitfields;
    pub const CSR_SSTATUS: u16 = 0x100;
    pub const CSR_SEDELEG: u16 = 0x102;
    pub const CSR_SIDELEG: u16 = 0x103;
    pub const CSR_SIE: u16 = 0x104;
    pub const CSR_STVEC: u16 = 0x105;
    pub const CSR_SCOUNTEREN: u16 = 0x106;
    pub const CSR_SENVCFG: u16 = 0x10a;
    pub const CSR_SSCRATCH: u16 = 0x140;
    pub const CSR_SEPC: u16 = 0x141;
    pub const CSR_SCAUSE: u16 = 0x142;
    pub const CSR_STVAL: u16 = 0x143;
    pub const CSR_SIP: u16 = 0x144;
    pub const CSR_STIMECMP: u16 = 0x14d;
    pub const CSR_SISELECT: u16 = 0x150;
    pub const CSR_SIREG: u16 = 0x151;
    pub const CSR_STOPEI: u16 = 0x15c;
    pub const CSR_SATP: u16 = 0x180;
    pub const CSR_STOPI: u16 = 0xdb0;
    pub const CSR_SCONTEXT: u16 = 0x5a8;
    pub const CSR_VSSTATUS: u16 = 0x200;
    pub const CSR_VSIE: u16 = 0x204;
    pub const CSR_VSTVEC: u16 = 0x205;
    pub const CSR_VSSCRATCH: u16 = 0x240;
    pub const CSR_VSEPC: u16 = 0x241;
    pub const CSR_VSCAUSE: u16 = 0x242;
    pub const CSR_VSTVAL: u16 = 0x243;
    pub const CSR_VSIP: u16 = 0x244;
    pub const CSR_VSTIMECMP: u16 = 0x24d;
    pub const CSR_VSISELECT: u16 = 0x250;
    pub const CSR_VSIREG: u16 = 0x251;
    pub const CSR_VSTOPEI: u16 = 0x25c;
    pub const CSR_VSATP: u16 = 0x280;
    pub const CSR_VSTOPI: u16 = 0xeb0;
    pub const CSR_HSTATUS: u16 = 0x600;
    pub const CSR_HEDELEG: u16 = 0x602;
    pub const CSR_HIDELEG: u16 = 0x603;
    pub const CSR_HIE: u16 = 0x604;
    pub const CSR_HTIMEDELTA: u16 = 0x605;
    pub const CSR_HCOUNTEREN: u16 = 0x606;
    pub const CSR_HGEIE: u16 = 0x607;
    pub const CSR_HVICTL: u16 = 0x609;
    pub const CSR_HENVCFG: u16 = 0x60a;
    pub const CSR_HTVAL: u16 = 0x643;
    pub const CSR_HIP: u16 = 0x644;
    pub const CSR_HVIP: u16 = 0x645;
    pub const CSR_HTINST: u16 = 0x64a;
    pub const CSR_HGATP: u16 = 0x680;
    pub const CSR_HCONTEXT: u16 = 0x6a8;
    pub const CSR_HGEIP: u16 = 0xe12;

    // Hypervisor exception delegation register.
    register_bitfields![usize,
    pub hedeleg [
        instr_misaligned OFFSET(0) NUMBITS(1) [],
        instr_fault OFFSET(1) NUMBITS(1) [],
        illegal_instr OFFSET(2) NUMBITS(1) [],
        breakpoint OFFSET(3) NUMBITS(1) [],
        load_misaligned OFFSET(4) NUMBITS(1) [],
        load_fault OFFSET(5) NUMBITS(1) [],
        store_misaligned OFFSET(6) NUMBITS(1) [],
        store_fault OFFSET(7) NUMBITS(1) [],
        u_ecall OFFSET(8) NUMBITS(1) [],
        instr_page_fault OFFSET(12) NUMBITS(1) [],
        load_page_fault OFFSET(13) NUMBITS(1) [],
        store_page_fault OFFSET(15) NUMBITS(1) [],
    ]
    ];

    // Supervisor interrupt enable register.
    register_bitfields![usize,
    pub sie [
        ssoft OFFSET(1) NUMBITS(1) [],
        stimer OFFSET(5) NUMBITS(1) [],
        sext OFFSET(9) NUMBITS(1) [],
    ]
    ];

    // Hypervisor status register.
    register_bitfields![usize,
    pub hstatus [
        // VS mode endianness control.
        vsbe OFFSET(6) NUMBITS(1) [],
        // A guest virtual address was written to stval as a result of the trap.
        gva OFFSET(6) NUMBITS(1) [],
        // Virtualization mode at time of trap.
        spv OFFSET(7) NUMBITS(1) [
            User = 0,
            Supervisor = 1,
        ],
        // Privilege level the virtual hart was executing before entering HS-mode.
        spvp OFFSET(8) NUMBITS(1) [
            User = 0,
            Supervisor = 1,
        ],
        // Allow hypervisor instructions in U-mode.
        hu OFFSET(9) NUMBITS(1) [],
        // Selects the guest external interrupt source for VS external interrupts.
        vgein OFFSET(12) NUMBITS(6) [],
        // Trap on SFENCE, SINVAL, or changes to vsatp.
        vtvm OFFSET(20) NUMBITS(1) [],
        // Trap on WFI timeout.
        vtw OFFSET(21) NUMBITS(1) [],
        // Trap SRET instruction.
        vtsr OFFSET(22) NUMBITS(1) [],
        // Native base integer ISA width for VS-mode.
        vsxl OFFSET(32) NUMBITS(2) [
            Xlen32 = 1,
            Xlen64 = 2,
        ],
    ]
    ];

    // Hypervisor interrupt delegation register.
    register_bitfields![usize,
    pub hideleg [
        vssoft OFFSET(2) NUMBITS(1) [],
        vstimer OFFSET(6) NUMBITS(1) [],
        vsext OFFSET(10) NUMBITS(1) [],
    ]
    ];

    // Hypervisor interrupt enable register.
    register_bitfields![usize,
    pub hie [
        vssoft OFFSET(2) NUMBITS(1) [],
        vstimer OFFSET(6) NUMBITS(1) [],
        vsext OFFSET(10) NUMBITS(1) [],
        sgext OFFSET(12) NUMBITS(1) [],
    ]
    ];

    // VS-mode counter availability control.
    register_bitfields![usize,
    pub hcounteren [
        cycle OFFSET(0) NUMBITS(1) [],
        time OFFSET(1) NUMBITS(1) [],
        instret OFFSET(2) NUMBITS(1) [],
        hpm OFFSET(3) NUMBITS(29) [],
    ]
    ];

    // Hypervisor virtual interrupt pending.
    register_bitfields![usize,
    pub hvip [
        vssoft OFFSET(2) NUMBITS(1) [],
        vstimer OFFSET(6) NUMBITS(1) [],
        vsext OFFSET(10) NUMBITS(1) [],
    ]
    ];
}

pub mod traps {
    pub mod interrupt {
        pub const USER_SOFT: usize = 1 << 0;
        pub const SUPERVISOR_SOFT: usize = 1 << 1;
        pub const VIRTUAL_SUPERVISOR_SOFT: usize = 1 << 2;
        pub const MACHINE_SOFT: usize = 1 << 3;
        pub const USER_TIMER: usize = 1 << 4;
        pub const SUPERVISOR_TIMER: usize = 1 << 5;
        pub const VIRTUAL_SUPERVISOR_TIMER: usize = 1 << 6;
        pub const MACHINE_TIMER: usize = 1 << 7;
        pub const USER_EXTERNAL: usize = 1 << 8;
        pub const SUPERVISOR_EXTERNAL: usize = 1 << 9;
        pub const VIRTUAL_SUPERVISOR_EXTERNAL: usize = 1 << 10;
        pub const MACHINEL_EXTERNAL: usize = 1 << 11;
        pub const SUPERVISOR_GUEST_EXTERNEL: usize = 1 << 12;
    }

    pub mod exception {
        pub const INST_ADDR_MISALIGN: usize = 1 << 0;
        pub const INST_ACCESSS_FAULT: usize = 1 << 1;
        pub const ILLEGAL_INST: usize = 1 << 2;
        pub const BREAKPOINT: usize = 1 << 3;
        pub const LOAD_ADDR_MISALIGNED: usize = 1 << 4;
        pub const LOAD_ACCESS_FAULT: usize = 1 << 5;
        pub const STORE_ADDR_MISALIGNED: usize = 1 << 6;
        pub const STORE_ACCESS_FAULT: usize = 1 << 7;
        pub const ENV_CALL_FROM_U_OR_VU: usize = 1 << 8;
        pub const ENV_CALL_FROM_HS: usize = 1 << 9;
        pub const ENV_CALL_FROM_VS: usize = 1 << 10;
        pub const ENV_CALL_FROM_M: usize = 1 << 11;
        pub const INST_PAGE_FAULT: usize = 1 << 12;
        pub const LOAD_PAGE_FAULT: usize = 1 << 13;
        pub const STORE_PAGE_FAULT: usize = 1 << 15;
        pub const INST_GUEST_PAGE_FAULT: usize = 1 << 20;
        pub const LOAD_GUEST_PAGE_FAULT: usize = 1 << 21;
        pub const VIRTUAL_INST: usize = 1 << 22;
        pub const STORE_GUEST_PAGE_FAULT: usize = 1 << 23;
    }
}
