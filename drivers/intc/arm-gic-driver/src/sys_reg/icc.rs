// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// ICC (Interrupt Controller CPU interface) System registers

// System Register Enable 寄存器
define_readwrite_register! {
    ICC_SRE_EL1 {
        SRE OFFSET(0) NUMBITS(1) [],
        DFB OFFSET(1) NUMBITS(1) [],
        DIB OFFSET(2) NUMBITS(1) [],
    }
}

define_readwrite_register! {
    ICC_SRE_EL2 {
        SRE OFFSET(0) NUMBITS(1) [],
        DFB OFFSET(1) NUMBITS(1) [],
        DIB OFFSET(2) NUMBITS(1) [],
        ENABLE OFFSET(3) NUMBITS(1) [],
    }
}

define_readwrite_register! {
    ICC_SRE_EL3 {
        SRE OFFSET(0) NUMBITS(1) [],
        DFB OFFSET(1) NUMBITS(1) [],
        DIB OFFSET(2) NUMBITS(1) [],
        ENABLE OFFSET(3) NUMBITS(1) [],
    }
}

// Interrupt Group Enable 寄存器
define_readwrite_register! {
    ICC_IGRPEN0_EL1 {
        ENABLE OFFSET(0) NUMBITS(1) [],
    }
}

define_readwrite_register! {
    ICC_IGRPEN1_EL1 {
        ENABLE OFFSET(0) NUMBITS(1) [],
    }
}

define_readwrite_register! {
    ICC_IGRPEN0_EL2 {
        ENABLE OFFSET(0) NUMBITS(1) [],
    }
}

define_readwrite_register! {
    ICC_IGRPEN1_EL2 {
        ENABLE OFFSET(0) NUMBITS(1) [],
    }
}

define_readwrite_register! {
    ICC_IGRPEN1_EL3 {
        ENABLE OFFSET(0) NUMBITS(1) [],
    }
}

// Control 寄存器
define_readwrite_register! {
    ICC_CTLR_EL1 {
        CBPR OFFSET(0) NUMBITS(1) [],
        EOIMODE OFFSET(1) NUMBITS(1) [],
        PMHE OFFSET(6) NUMBITS(1) [],
        PRIBITS OFFSET(8) NUMBITS(3) [],
        IDBITS OFFSET(11) NUMBITS(4) [],
        SEIS OFFSET(14) NUMBITS(1) [],
        A3V OFFSET(15) NUMBITS(1) [],
        RSS OFFSET(18) NUMBITS(1) [],
        EXTRANGE OFFSET(19) NUMBITS(1) [],
    }
}

define_readwrite_register! {
    ICC_CTLR_EL3 {
        CBPR OFFSET(0) NUMBITS(1) [],
        EOIMODE OFFSET(1) NUMBITS(1) [],
        PMHE OFFSET(6) NUMBITS(1) [],
        PRIBITS OFFSET(8) NUMBITS(3) [],
        IDBITS OFFSET(11) NUMBITS(4) [],
        SEIS OFFSET(14) NUMBITS(1) [],
        A3V OFFSET(15) NUMBITS(1) [],
        NDSEIS OFFSET(17) NUMBITS(1) [],
        RSS OFFSET(18) NUMBITS(1) [],
        EXTRANGE OFFSET(19) NUMBITS(1) [],
    }
}

// Interrupt Acknowledge 寄存器
define_readonly_register! {
    ICC_IAR0_EL1 {
        INTID OFFSET(0) NUMBITS(24) [],
    }
}

define_readonly_register! {
    ICC_IAR1_EL1 {
        INTID OFFSET(0) NUMBITS(24) [],
    }
}

// End of Interrupt 寄存器
define_writeonly_register! {
    ICC_EOIR0_EL1 {
        INTID OFFSET(0) NUMBITS(24) [],
    }
}

define_writeonly_register! {
    ICC_EOIR1_EL1 {
        INTID OFFSET(0) NUMBITS(24) [],
    }
}

// Highest Priority Pending Interrupt 寄存器
define_readonly_register! {
    ICC_HPPIR0_EL1 {
        INTID OFFSET(0) NUMBITS(24) [],
    }
}

define_readonly_register! {
    ICC_HPPIR1_EL1 {
        INTID OFFSET(0) NUMBITS(24) [],
    }
}

// Binary Point 寄存器
define_readwrite_register! {
    ICC_BPR0_EL1 {
        BINARYPOINT OFFSET(0) NUMBITS(3) [],
    }
}

define_readwrite_register! {
    ICC_BPR1_EL1 {
        BINARYPOINT OFFSET(0) NUMBITS(3) [],
    }
}

// Priority Mask 寄存器
define_readwrite_register! {
    ICC_PMR_EL1 {
        PRIORITY OFFSET(0) NUMBITS(8) [],
    }
}

// Running Priority 寄存器
define_readonly_register! {
    ICC_RPR_EL1 {
        PRIORITY OFFSET(0) NUMBITS(8) [],
    }
}

// Active Priority Group 0 寄存器
define_readwrite_register! {
    ICC_AP0R0_EL1 {
        ACTIVE OFFSET(0) NUMBITS(32) [],
    }
}

define_readwrite_register! {
    ICC_AP0R1_EL1 {
        ACTIVE OFFSET(0) NUMBITS(32) [],
    }
}

define_readwrite_register! {
    ICC_AP0R2_EL1 {
        ACTIVE OFFSET(0) NUMBITS(32) [],
    }
}

define_readwrite_register! {
    ICC_AP0R3_EL1 {
        ACTIVE OFFSET(0) NUMBITS(32) [],
    }
}

// Active Priority Group 1 寄存器
define_readwrite_register! {
    ICC_AP1R0_EL1 {
        NMI OFFSET(63) NUMBITS(1) [],
        ACTIVE OFFSET(0) NUMBITS(32) [],
    }
}

define_readwrite_register! {
    ICC_AP1R1_EL1 {
        ACTIVE OFFSET(0) NUMBITS(32) [],
    }
}

define_readwrite_register! {
    ICC_AP1R2_EL1 {
        ACTIVE OFFSET(0) NUMBITS(32) [],
    }
}

define_readwrite_register! {
    ICC_AP1R3_EL1 {
        ACTIVE OFFSET(0) NUMBITS(32) [],
    }
}

// Deactivate Interrupt 寄存器
define_writeonly_register! {
    ICC_DIR_EL1 {
        INTID OFFSET(0) NUMBITS(24) [],
    }
}

// Software Generated Interrupt 寄存器
define_writeonly_register! {
    ICC_SGI0R_EL1 {
        TARGETLIST OFFSET(0) NUMBITS(16) [],
        AFF1 OFFSET(16) NUMBITS(8) [],
        INTID OFFSET(24) NUMBITS(4) [],
        AFF2 OFFSET(32) NUMBITS(8) [],
        IRM OFFSET(40) NUMBITS(1) [],
        RS OFFSET(44) NUMBITS(4) [],
        AFF3 OFFSET(48) NUMBITS(8) [],
    }
}

define_writeonly_register! {
    ICC_SGI1R_EL1 {
        TARGETLIST OFFSET(0) NUMBITS(16) [],
        AFF1 OFFSET(16) NUMBITS(8) [],
        INTID OFFSET(24) NUMBITS(4) [],
        AFF2 OFFSET(32) NUMBITS(8) [],
        IRM OFFSET(40) NUMBITS(1) [],
        RS OFFSET(44) NUMBITS(4) [],
        AFF3 OFFSET(48) NUMBITS(8) [],
    }
}

define_writeonly_register! {
    ICC_ASGI1R_EL1 {
        TARGETLIST OFFSET(0) NUMBITS(16) [],
        AFF1 OFFSET(16) NUMBITS(8) [],
        INTID OFFSET(24) NUMBITS(4) [],
        AFF2 OFFSET(32) NUMBITS(8) [],
        IRM OFFSET(40) NUMBITS(1) [],
        RS OFFSET(44) NUMBITS(4) [],
        AFF3 OFFSET(48) NUMBITS(8) [],
    }
}
