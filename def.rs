/// Definitions
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
    #[allow(non_snake_case)]
    /// Hypervisor exception delegation register.
    pub mod hedeleg {
        pub struct Register;
        #[automatically_derived]
        impl ::core::clone::Clone for Register {
            #[inline]
            fn clone(&self) -> Register {
                *self
            }
        }
        #[automatically_derived]
        impl ::core::marker::Copy for Register {}
        impl ::tock_registers::RegisterLongName for Register {}
        use ::tock_registers::fields::Field;
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const instr_misaligned: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 0);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Instruction address misaligned.
        pub mod instr_misaligned {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                0,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 0, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Instruction address misaligned.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const instr_fault: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 1);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Instruction access fault.
        pub mod instr_fault {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                1,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 1, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Instruction access fault.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const illegal_instr: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 2);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Illegal instruction.
        pub mod illegal_instr {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                2,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 2, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Illegal instruction.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const breakpoint: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 3);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Breakpoint.
        pub mod breakpoint {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                3,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 3, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Breakpoint.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const load_misaligned: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 4);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Load address misaligned.
        pub mod load_misaligned {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                4,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 4, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Load address misaligned.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const load_fault: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 5);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Load access fault.
        pub mod load_fault {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                5,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 5, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Load access fault.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const store_misaligned: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Store address misaligned.
        pub mod store_misaligned {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                6,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Store address misaligned.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const store_fault: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 7);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Store access fault.
        pub mod store_fault {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                7,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 7, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Store access fault.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const u_ecall: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 8);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// User environment call.
        pub mod u_ecall {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                8,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 8, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// User environment call.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const instr_page_fault: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 12);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Instruction page fault.
        pub mod instr_page_fault {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                12,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 12, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Instruction page fault.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const load_page_fault: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 13);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Load page fault.
        pub mod load_page_fault {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                13,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 13, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Load page fault.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const store_page_fault: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 15);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Store page fault.
        pub mod store_page_fault {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                15,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 15, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Store page fault.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
    }
    #[allow(non_snake_case)]
    /// Supervisor interrupt enable register.
    pub mod sie {
        pub struct Register;
        #[automatically_derived]
        impl ::core::clone::Clone for Register {
            #[inline]
            fn clone(&self) -> Register {
                *self
            }
        }
        #[automatically_derived]
        impl ::core::marker::Copy for Register {}
        impl ::tock_registers::RegisterLongName for Register {}
        use ::tock_registers::fields::Field;
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const ssoft: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 1);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Supervisor software interrupt.
        pub mod ssoft {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                1,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 1, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Supervisor software interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const stimer: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 5);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Supervisor timer interrupt.
        pub mod stimer {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                5,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 5, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Supervisor timer interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const sext: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 9);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Supervisor external interrupt.
        pub mod sext {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                9,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 9, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Supervisor external interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
    }
    #[allow(non_snake_case)]
    /// Hypervisor status register.
    pub mod hstatus {
        pub struct Register;
        #[automatically_derived]
        impl ::core::clone::Clone for Register {
            #[inline]
            fn clone(&self) -> Register {
                *self
            }
        }
        #[automatically_derived]
        impl ::core::marker::Copy for Register {}
        impl ::tock_registers::RegisterLongName for Register {}
        use ::tock_registers::fields::Field;
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vsbe: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6);
        #[allow(non_snake_case)]
        #[allow(unused)]
        pub mod vsbe {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                6,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const gva: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6);
        #[allow(non_snake_case)]
        #[allow(unused)]
        pub mod gva {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                6,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const spv: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 7);
        #[allow(non_snake_case)]
        #[allow(unused)]
        pub mod spv {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{TryFromValue, FieldValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            /// User mode.
            pub const User: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 7, 0);
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            /// Supervisor mode.
            pub const Supervisor: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 7, 1);
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                7,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 7, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            #[repr(usize)]
            pub enum Value {
                /// User mode.
                User = 0,
                /// Supervisor mode.
                Supervisor = 1,
            }
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::marker::Copy for Value {}
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::clone::Clone for Value {
                #[inline]
                fn clone(&self) -> Value {
                    *self
                }
            }
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::cmp::Eq for Value {
                #[inline]
                #[doc(hidden)]
                #[coverage(off)]
                fn assert_receiver_is_total_eq(&self) -> () {}
            }
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::marker::StructuralPartialEq for Value {}
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::cmp::PartialEq for Value {
                #[inline]
                fn eq(&self, other: &Value) -> bool {
                    let __self_discr = ::core::intrinsics::discriminant_value(self);
                    let __arg1_discr = ::core::intrinsics::discriminant_value(other);
                    __self_discr == __arg1_discr
                }
            }
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(v: usize) -> Option<Self::EnumType> {
                    match v {
                        /// User mode.
                        x if x == Value::User as usize => Some(Value::User),
                        /// Supervisor mode.
                        x if x == Value::Supervisor as usize => Some(Value::Supervisor),
                        _ => Option::None,
                    }
                }
            }
            impl From<Value> for FieldValue<usize, Register> {
                fn from(v: Value) -> Self {
                    Self::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 7, v as usize)
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const spvp: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 8);
        #[allow(non_snake_case)]
        #[allow(unused)]
        pub mod spvp {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{TryFromValue, FieldValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            /// User mode.
            pub const User: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 8, 0);
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            /// Supervisor mode.
            pub const Supervisor: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 8, 1);
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                8,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 8, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            #[repr(usize)]
            pub enum Value {
                /// User mode.
                User = 0,
                /// Supervisor mode.
                Supervisor = 1,
            }
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::marker::Copy for Value {}
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::clone::Clone for Value {
                #[inline]
                fn clone(&self) -> Value {
                    *self
                }
            }
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::cmp::Eq for Value {
                #[inline]
                #[doc(hidden)]
                #[coverage(off)]
                fn assert_receiver_is_total_eq(&self) -> () {}
            }
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::marker::StructuralPartialEq for Value {}
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::cmp::PartialEq for Value {
                #[inline]
                fn eq(&self, other: &Value) -> bool {
                    let __self_discr = ::core::intrinsics::discriminant_value(self);
                    let __arg1_discr = ::core::intrinsics::discriminant_value(other);
                    __self_discr == __arg1_discr
                }
            }
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(v: usize) -> Option<Self::EnumType> {
                    match v {
                        /// User mode.
                        x if x == Value::User as usize => Some(Value::User),
                        /// Supervisor mode.
                        x if x == Value::Supervisor as usize => Some(Value::Supervisor),
                        _ => Option::None,
                    }
                }
            }
            impl From<Value> for FieldValue<usize, Register> {
                fn from(v: Value) -> Self {
                    Self::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 8, v as usize)
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const hu: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 9);
        #[allow(non_snake_case)]
        #[allow(unused)]
        pub mod hu {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                9,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 9, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vgein: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (6 - 1)) + ((1 << (6 - 1)) - 1), 12);
        #[allow(non_snake_case)]
        #[allow(unused)]
        pub mod vgein {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (6 - 1)) + ((1 << (6 - 1)) - 1),
                12,
                (1 << (6 - 1)) + ((1 << (6 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (6 - 1)) + ((1 << (6 - 1)) - 1), 12, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vtvm: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 20);
        #[allow(non_snake_case)]
        #[allow(unused)]
        pub mod vtvm {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                20,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 20, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vtw: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 21);
        #[allow(non_snake_case)]
        #[allow(unused)]
        pub mod vtw {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                21,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 21, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vtsr: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 22);
        #[allow(non_snake_case)]
        #[allow(unused)]
        pub mod vtsr {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                22,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 22, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vsxl: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (2 - 1)) + ((1 << (2 - 1)) - 1), 32);
        #[allow(non_snake_case)]
        #[allow(unused)]
        pub mod vsxl {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{TryFromValue, FieldValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            /// 32-bit.
            pub const Xlen32: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (2 - 1)) + ((1 << (2 - 1)) - 1), 32, 1);
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            /// 64-bit.
            pub const Xlen64: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (2 - 1)) + ((1 << (2 - 1)) - 1), 32, 2);
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (2 - 1)) + ((1 << (2 - 1)) - 1),
                32,
                (1 << (2 - 1)) + ((1 << (2 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (2 - 1)) + ((1 << (2 - 1)) - 1), 32, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            #[repr(usize)]
            pub enum Value {
                /// 32-bit.
                Xlen32 = 1,
                /// 64-bit.
                Xlen64 = 2,
            }
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::marker::Copy for Value {}
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::clone::Clone for Value {
                #[inline]
                fn clone(&self) -> Value {
                    *self
                }
            }
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::cmp::Eq for Value {
                #[inline]
                #[doc(hidden)]
                #[coverage(off)]
                fn assert_receiver_is_total_eq(&self) -> () {}
            }
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::marker::StructuralPartialEq for Value {}
            #[automatically_derived]
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            impl ::core::cmp::PartialEq for Value {
                #[inline]
                fn eq(&self, other: &Value) -> bool {
                    let __self_discr = ::core::intrinsics::discriminant_value(self);
                    let __arg1_discr = ::core::intrinsics::discriminant_value(other);
                    __self_discr == __arg1_discr
                }
            }
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(v: usize) -> Option<Self::EnumType> {
                    match v {
                        /// 32-bit.
                        x if x == Value::Xlen32 as usize => Some(Value::Xlen32),
                        /// 64-bit.
                        x if x == Value::Xlen64 as usize => Some(Value::Xlen64),
                        _ => Option::None,
                    }
                }
            }
            impl From<Value> for FieldValue<usize, Register> {
                fn from(v: Value) -> Self {
                    Self::new((1 << (2 - 1)) + ((1 << (2 - 1)) - 1), 32, v as usize)
                }
            }
        }
    }
    #[allow(non_snake_case)]
    /// Hypervisor interrupt delegation register.
    pub mod hideleg {
        pub struct Register;
        #[automatically_derived]
        impl ::core::clone::Clone for Register {
            #[inline]
            fn clone(&self) -> Register {
                *self
            }
        }
        #[automatically_derived]
        impl ::core::marker::Copy for Register {}
        impl ::tock_registers::RegisterLongName for Register {}
        use ::tock_registers::fields::Field;
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vssoft: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 2);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// VS-mode software interrupt.
        pub mod vssoft {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                2,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 2, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// VS-mode software interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vstimer: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// VS-mode timer interrupt.
        pub mod vstimer {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                6,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// VS-mode timer interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vsext: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 10);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// VS-mode external interrupt.
        pub mod vsext {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                10,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 10, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// VS-mode external interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
    }
    #[allow(non_snake_case)]
    /// Hypervisor interrupt enable register.
    pub mod hie {
        pub struct Register;
        #[automatically_derived]
        impl ::core::clone::Clone for Register {
            #[inline]
            fn clone(&self) -> Register {
                *self
            }
        }
        #[automatically_derived]
        impl ::core::marker::Copy for Register {}
        impl ::tock_registers::RegisterLongName for Register {}
        use ::tock_registers::fields::Field;
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vssoft: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 2);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// VS-mode software interrupt.
        pub mod vssoft {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                2,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 2, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// VS-mode software interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vstimer: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// VS-mode timer interrupt.
        pub mod vstimer {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                6,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// VS-mode timer interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vsext: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 10);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// VS-mode external interrupt.
        pub mod vsext {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                10,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 10, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// VS-mode external interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const sgext: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 12);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Supervisor guest external interrupt.
        pub mod sgext {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                12,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 12, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Supervisor guest external interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
    }
    #[allow(non_snake_case)]
    /// Hypervisor counter enable register.
    pub mod hcounteren {
        pub struct Register;
        #[automatically_derived]
        impl ::core::clone::Clone for Register {
            #[inline]
            fn clone(&self) -> Register {
                *self
            }
        }
        #[automatically_derived]
        impl ::core::marker::Copy for Register {}
        impl ::tock_registers::RegisterLongName for Register {}
        use ::tock_registers::fields::Field;
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const cycle: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 0);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Cycle.
        pub mod cycle {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                0,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 0, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Cycle.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const time: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 1);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Time.
        pub mod time {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                1,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 1, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Time.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const instret: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 2);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// Instret.
        pub mod instret {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                2,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 2, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// Instret.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const hpm: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (29 - 1)) + ((1 << (29 - 1)) - 1), 3);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// HPM.
        pub mod hpm {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (29 - 1)) + ((1 << (29 - 1)) - 1),
                3,
                (1 << (29 - 1)) + ((1 << (29 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (29 - 1)) + ((1 << (29 - 1)) - 1), 3, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// HPM.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
    }
    #[allow(non_snake_case)]
    /// Hypervisor virtual interrupt pending.
    pub mod hvip {
        pub struct Register;
        #[automatically_derived]
        impl ::core::clone::Clone for Register {
            #[inline]
            fn clone(&self) -> Register {
                *self
            }
        }
        #[automatically_derived]
        impl ::core::marker::Copy for Register {}
        impl ::tock_registers::RegisterLongName for Register {}
        use ::tock_registers::fields::Field;
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vssoft: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 2);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// VS-mode software interrupt.
        pub mod vssoft {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                2,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 2, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// VS-mode software interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vstimer: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// VS-mode timer interrupt.
        pub mod vstimer {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                6,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 6, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// VS-mode timer interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
        #[allow(non_upper_case_globals)]
        #[allow(unused)]
        pub const vsext: Field<usize, Register> = Field::<
            usize,
            Register,
        >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 10);
        #[allow(non_snake_case)]
        #[allow(unused)]
        /// VS-mode external interrupt.
        pub mod vsext {
            #[allow(unused_imports)]
            use ::tock_registers::fields::{FieldValue, TryFromValue};
            use super::Register;
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const SET: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new(
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
                10,
                (1 << (1 - 1)) + ((1 << (1 - 1)) - 1),
            );
            #[allow(non_upper_case_globals)]
            #[allow(unused)]
            pub const CLEAR: FieldValue<usize, Register> = FieldValue::<
                usize,
                Register,
            >::new((1 << (1 - 1)) + ((1 << (1 - 1)) - 1), 10, 0);
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            /// VS-mode external interrupt.
            pub enum Value {}
            impl TryFromValue<usize> for Value {
                type EnumType = Value;
                fn try_from_value(_v: usize) -> Option<Self::EnumType> {
                    Option::None
                }
            }
        }
    }
}
