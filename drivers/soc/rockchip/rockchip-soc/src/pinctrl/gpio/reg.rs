use tock_registers::{register_structs, registers::*};

register_structs! {
    pub Registers {
        // Port Data Register (0x0000-0x0004)
        (0x00 => pub swport_dr_l: ReadWrite<u32>),
        (0x04 => pub swport_dr_h: ReadWrite<u32>),

        // Port Data Direction Register (0x0008-0x000C)
        (0x08 => pub swport_ddr_l: ReadWrite<u32>),
        (0x0c => pub swport_ddr_h: ReadWrite<u32>),

        // Interrupt Enable Register (0x0010-0x0014)
        (0x10 => pub int_en_l: ReadWrite<u32>),
        (0x14 => pub int_en_h: ReadWrite<u32>),

        // Interrupt Mask Register (0x0018-0x001C)
        (0x18 => pub int_mask_l: ReadWrite<u32>),
        (0x1c => pub int_mask_h: ReadWrite<u32>),

        // Interrupt Level Register (0x0020-0x0024)
        (0x20 => pub int_type_l: ReadWrite<u32>),
        (0x24 => pub int_type_h: ReadWrite<u32>),

        // Interrupt Polarity Register (0x0028-0x002C)
        (0x28 => pub int_polarity_l: ReadWrite<u32>),
        (0x2c => pub int_polarity_h: ReadWrite<u32>),

        // Interrupt Both Edge Type Register (0x0030-0x0034)
        (0x30 => pub int_bothedge_l: ReadWrite<u32>),
        (0x34 => pub int_bothedge_h: ReadWrite<u32>),

        // Debounce Enable Register (0x0038-0x003C)
        (0x38 => pub debounce_l: ReadWrite<u32>),
        (0x3c => pub debounce_h: ReadWrite<u32>),

        // DBCLK Divide Enable Register (0x0040-0x0044)
        (0x40 => pub dbclk_div_en_l: ReadWrite<u32>),
        (0x44 => pub dbclk_div_en_h: ReadWrite<u32>),

        // DBCLK Divide Control Register (0x0048)
        (0x48 => pub dbclk_div_con: ReadWrite<u32>),

        (0x4c => _rsv0),

        // Interrupt Status Register (0x0050)
        (0x50 => pub int_status: ReadWrite<u32>),

        (0x54 => _rsv1),

        // Interrupt Raw Status Register (0x0058)
        (0x58 => pub int_rawstatus: ReadWrite<u32>),

        (0x5c => _rsv2),

        // Interrupt Clear Register (0x0060-0x0064)
        (0x60 => pub port_eoi_l: ReadWrite<u32>),
        (0x64 => pub port_eoi_h: ReadWrite<u32>),

        (0x68 => _rsv3),

        // External Port Data Register (0x0070)
        (0x70 => pub ext_port: ReadWrite<u32>),

        (0x74 => _rsv4),

        // Version ID Register (0x0078)
        (0x78 => pub ver_id: ReadOnly<u32>),
        (0x7c => _rsv5),
        // GPIO Group Control (0x0100-0x0104)
        (0x100 => pub gpio_reg_group_l: ReadWrite<u32>),
        (0x104 => pub gpio_reg_group_h: ReadWrite<u32>),

        // GPIO Virtual Enable (0x0108)
        (0x108 => pub gpio_virtual_en: ReadWrite<u32>),

        (0x10c => @END),
    }
}
