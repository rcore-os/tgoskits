use tock_registers::{
    register_structs,
    registers::{ReadOnly, ReadWrite},
};

register_structs! {
    #[allow(non_snake_case)]
    pub PpuRdmaRegs {
        (0x0000 => pub s_status: ReadOnly<u32>),
        (0x0004 => pub s_pointer: ReadWrite<u32>),
        (0x0008 => pub operation_enable: ReadWrite<u32>),
        (0x000C => pub cube_in_width: ReadWrite<u32>),
        (0x0010 => pub cube_in_height: ReadWrite<u32>),
        (0x0014 => pub cube_in_channel: ReadWrite<u32>),
        (0x0018 => pub src_base_addr: ReadWrite<u32>),
        (0x001C => pub src_line_stride: ReadWrite<u32>),
        (0x0020 => pub src_surf_stride: ReadWrite<u32>),
        (0x0024 => pub data_format: ReadWrite<u32>),
        (0x0028 => pub outstanding_cfg: ReadWrite<u32>),
        (0x002C => pub rd_weight0: ReadWrite<u32>),
        (0x0030 => pub wr_weight0: ReadWrite<u32>),
        (0x0034 => pub cfg_id_error: ReadWrite<u32>),
        (0x0038 => pub rd_weight1: ReadWrite<u32>),
        (0x003C => pub cfg_dma_fifo_clr: ReadWrite<u32>),
        (0x0040 => pub cfg_dma_arb: ReadWrite<u32>),
        (0x0044 => pub cfg_dma_rd_qos: ReadWrite<u32>),
        (0x0048 => pub cfg_dma_rd_cfg: ReadWrite<u32>),
        (0x004C => pub cfg_dma_wr_cfg: ReadWrite<u32>),
        (0x0050 => pub cfg_dma_wstrb: ReadWrite<u32>),
        (0x0054 => pub cfg_status: ReadWrite<u32>),
        (0x0058 => @END),
    }
}
