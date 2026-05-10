use tock_registers::{
    register_structs,
    registers::{ReadOnly, ReadWrite},
};

register_structs! {
    #[allow(non_snake_case)]
    pub DpuRdmaRegs {
        (0x0000 => pub s_status: ReadOnly<u32>),
        (0x0004 => pub s_pointer: ReadWrite<u32>),
        (0x0008 => pub operation_enable: ReadWrite<u32>),
        (0x000C => _reserved0),
        (0x0010 => pub data_cube_width: ReadWrite<u32>),
        (0x0014 => pub data_cube_height: ReadWrite<u32>),
        (0x0018 => pub data_cube_channel: ReadWrite<u32>),
        (0x001C => pub src_base_addr: ReadWrite<u32>),
        (0x0020 => pub brdma_cfg: ReadWrite<u32>),
        (0x0024 => pub src_dma_cfg: ReadWrite<u32>),
        (0x0028 => pub surf_notch: ReadWrite<u32>),
        (0x002C => pub pad_cfg: ReadWrite<u32>),
        (0x0030 => pub weight: ReadWrite<u32>),
        (0x0034 => pub ew_surf_notch: ReadWrite<u32>),
        (0x0038 => pub feature_mode_cfg: ReadWrite<u32>),
        (0x003C => pub src_dma_cfg_ext: ReadWrite<u32>),
        (0x0040 => @END),
    }
}
