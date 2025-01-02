/// Constants about traps.
#[allow(dead_code)]
pub mod traps {
    /// Constants about interrupt.
    pub mod interrupt {
        /// User software interrupt.
        pub const USER_SOFT: usize = 1 << 0;
        /// Supervisor software interrupt.
        pub const SUPERVISOR_SOFT: usize = 1 << 1;
        /// Virtual supervisor software interrupt.
        pub const VIRTUAL_SUPERVISOR_SOFT: usize = 1 << 2;
        /// Machine software interrupt.
        pub const MACHINE_SOFT: usize = 1 << 3;
        /// User timer interrupt.
        pub const USER_TIMER: usize = 1 << 4;
        /// Supervisor timer interrupt.
        pub const SUPERVISOR_TIMER: usize = 1 << 5;
        /// Virtual supervisor timer interrupt.
        pub const VIRTUAL_SUPERVISOR_TIMER: usize = 1 << 6;
        /// Machine timer interrupt.
        pub const MACHINE_TIMER: usize = 1 << 7;
        /// User external interrupt.
        pub const USER_EXTERNAL: usize = 1 << 8;
        /// Supervisor external interrupt.
        pub const SUPERVISOR_EXTERNAL: usize = 1 << 9;
        /// Virtual supervisor external interrupt.
        pub const VIRTUAL_SUPERVISOR_EXTERNAL: usize = 1 << 10;
        /// Machine external interrupt.
        pub const MACHINEL_EXTERNAL: usize = 1 << 11;
        /// Supervisor guest external interrupt.
        pub const SUPERVISOR_GUEST_EXTERNEL: usize = 1 << 12;
    }

    /// Constants about exception.
    pub mod exception {
        /// Instruction address misaligned.
        pub const INST_ADDR_MISALIGN: usize = 1 << 0;
        /// Instruction access fault.
        pub const INST_ACCESSS_FAULT: usize = 1 << 1;
        /// Illegal instruction.
        pub const ILLEGAL_INST: usize = 1 << 2;
        /// Breakpoint.
        pub const BREAKPOINT: usize = 1 << 3;
        /// Load address misaligned.
        pub const LOAD_ADDR_MISALIGNED: usize = 1 << 4;
        /// Load access fault.
        pub const LOAD_ACCESS_FAULT: usize = 1 << 5;
        /// Store address misaligned.
        pub const STORE_ADDR_MISALIGNED: usize = 1 << 6;
        /// Store access fault.
        pub const STORE_ACCESS_FAULT: usize = 1 << 7;
        /// Environment call from U-mode or VU-mode.
        pub const ENV_CALL_FROM_U_OR_VU: usize = 1 << 8;
        /// Environment call from HS-mode.
        pub const ENV_CALL_FROM_HS: usize = 1 << 9;
        /// Environment call from VS-mode.
        pub const ENV_CALL_FROM_VS: usize = 1 << 10;
        /// Environment call from M-mode.
        pub const ENV_CALL_FROM_M: usize = 1 << 11;
        /// Instruction page fault.
        pub const INST_PAGE_FAULT: usize = 1 << 12;
        /// Load page fault.
        pub const LOAD_PAGE_FAULT: usize = 1 << 13;
        /// Store page fault.
        pub const STORE_PAGE_FAULT: usize = 1 << 15;
        /// Instruction guest page fault.
        pub const INST_GUEST_PAGE_FAULT: usize = 1 << 20;
        /// Load guest page fault.
        pub const LOAD_GUEST_PAGE_FAULT: usize = 1 << 21;
        /// Virtual instruction.
        pub const VIRTUAL_INST: usize = 1 << 22;
        /// Store guest page fault.
        pub const STORE_GUEST_PAGE_FAULT: usize = 1 << 23;
    }

    /// Constants about IRQ.
    pub mod irq {
        /// `Interrupt` bit in `scause`
        pub const INTC_IRQ_BASE: usize = 1 << (usize::BITS - 1);
        /// Supervisor software interrupt in `scause`
        pub const S_SOFT: usize = INTC_IRQ_BASE + 1;
        /// Supervisor timer interrupt in `scause`
        pub const S_TIMER: usize = INTC_IRQ_BASE + 5;
        /// Supervisor external interrupt in `scause`
        pub const S_EXT: usize = INTC_IRQ_BASE + 9;
        /// The maximum number of IRQs.
        pub const MAX_IRQ_COUNT: usize = 1024;
        /// The timer IRQ number (supervisor timer interrupt in `scause`).
        pub const TIMER_IRQ_NUM: usize = S_TIMER;
    }
}
