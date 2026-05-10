use tock_registers::{register_structs, registers::ReadWrite};

register_structs! {
    #[allow(non_snake_case)]
    pub DdmaRegs {
        (0x0000 => pub cfg_outstanding: ReadWrite<u32>),
        (0x0004 => pub rd_weight0: ReadWrite<u32>),
        (0x0008 => pub wr_weight0: ReadWrite<u32>),
        (0x000C => pub cfg_id_error: ReadWrite<u32>),
        (0x0010 => pub rd_weight1: ReadWrite<u32>),
        (0x0014 => pub cfg_dma_fifo_clr: ReadWrite<u32>),
        (0x0018 => pub cfg_dma_arb: ReadWrite<u32>),
        (0x001C => pub cfg_dma_rd_qos: ReadWrite<u32>),
        (0x0020 => pub cfg_dma_rd_cfg: ReadWrite<u32>),
        (0x0024 => pub cfg_dma_wr_cfg: ReadWrite<u32>),
        (0x0028 => pub cfg_dma_wstrb: ReadWrite<u32>),
        (0x002C => pub cfg_status: ReadWrite<u32>),
        (0x0030 => @END),
    }
}
