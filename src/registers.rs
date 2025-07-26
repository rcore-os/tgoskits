// Macro to define GIC register enums
macro_rules! generate_gic_registers {
    (
        // Single register definitions
        singles {
            $(
                $single_name:ident = $single_offset:expr // Single register name and offset
            ),* $(,)?
        }
        // Range register definitions
        ranges {
            $(
                $range_name:ident = {
                    offset: $range_offset:expr, // Range register base offset
                    size: $range_size:expr // Number of registers in the range
                }
            ),* $(,)?
        }
    ) => {
        #[allow(clippy::enum_variant_names)]
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub enum GicRegister {
            // Generate single register variants
            $(
                $single_name, // Single register variant
            )*
            // Generate range register variants (with index)
            $(
                $range_name(u32), // Range register variant with index
            )*
        }

        impl GicRegister {

            // Convert address to register enum
            pub fn from_addr(addr: u32) -> Option<Self> {
                match addr {
                    // Match single registers
                    $(
                        addr if addr == $single_offset => Some(Self::$single_name), // Single register match
                    )*
                    // Match range registers
                    $(
                        addr if ($range_offset..$range_offset + ($range_size * 4)).contains(&addr)   => {
                            let idx = (addr - $range_offset) / 4; // Calculate index
                            if idx < $range_size {
                                Some(Self::$range_name(idx)) // Range register match
                            } else {
                                None
                            }
                        },
                    )*
                    _ => None, // No match
                }
            }
        }
    };
}

// Use the macro to generate specific register definitions
generate_gic_registers! {
    singles {
        // Distributor Control Register
        GicdCtlr = 0x0000,
        // Distributor Type Register
        GicdTyper = 0x0004,
        // Distributor Implementer Identification Register
        GicdIidr = 0x0008,
        // Distributor Status Register
        GicdStatusr = 0x0010,
    }
    ranges {
        // Interrupt Group Register
        GicdIgroupr = {
            offset: 0x0080,
            size: 32
        },
        // Interrupt Enable Set Register
        GicdIsenabler = {
            offset: 0x0100,
            size: 32
        },
        // Interrupt Enable Clear Register
        GicdIcenabler = {
            offset: 0x0180,
            size: 32
        },
        // Interrupt Pending Set Register
        GicdIspendr = {
            offset: 0x0200,
            size: 32
        },
        GicdIcpendr = {
            offset: 0x0280,
            size: 32
        },
        // Interrupt Active Set Register
        GicdIsactiver = {
            offset: 0x0300,
            size: 32
        },
        // Interrupt Active Clear Register
        GicdIcactiver = {
            offset: 0x0380,
            size: 32
        },
        // Interrupt Priority Register
        GicdIpriorityr = {
            offset: 0x0400,
            size: 256
        },
        // Interrupt Target Register
        GicdItargetsr = {
            offset: 0x0800,
            size: 256
        },
        // Interrupt Configuration Register
        GicdIcfgr = {
            offset: 0x0c00,
            size: 64
        },
        // PPI Status Register
        GicdPpisr = {
            offset: 0x0d00,
            size: 32
        },
        // SPI Status Register
        GicdSpisr = {
            offset: 0x0d04,
            size: 32
        },
        // Non-Secure Access Control Register
        GicdNsacr = {
            offset: 0x0e00,
            size: 32
        },
        // Software Generated Interrupt Register
        GicdSgir = {
            offset: 0x0f00,
            size: 32
        },
        // Pending Software Generated Interrupt Register
        GicdCpendsgir = {
            offset: 0x0f10,
            size: 32
        },
        // Software Generated Interrupt Pending Register
        GicdSpendsgir = {
            offset: 0x0f20,
            size: 32
        },
    }
}
