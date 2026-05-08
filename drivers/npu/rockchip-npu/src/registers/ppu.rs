use tock_registers::{
    register_structs,
    registers::{ReadOnly, ReadWrite},
};

register_structs! {
    #[allow(non_snake_case)]
    pub PpuRegs {
        (0x0000 => pub s_status: ReadOnly<u32>),
        (0x0004 => pub s_pointer: ReadWrite<u32>),
        (0x0008 => pub operation_enable: ReadWrite<u32>),
        (0x000C => pub data_cube_in_width: ReadWrite<u32>),
        (0x0010 => pub data_cube_in_height: ReadWrite<u32>),
        (0x0014 => pub data_cube_in_channel: ReadWrite<u32>),
        (0x0018 => pub data_cube_out_width: ReadWrite<u32>),
        (0x001C => pub data_cube_out_height: ReadWrite<u32>),
        (0x0020 => pub data_cube_out_channel: ReadWrite<u32>),
        (0x0024 => pub padding_value_1: ReadWrite<u32>),
        (0x0028 => pub padding_value_2: ReadWrite<u32>),
        (0x002C => _reserved0),
        (0x0034 => pub operation_mode_cfg: ReadWrite<u32>),
        (0x0038 => pub pooling_kernel_cfg: ReadWrite<u32>),
        (0x003C => pub recip_kernel_width: ReadWrite<u32>),
        (0x0040 => pub recip_kernel_height: ReadWrite<u32>),
        (0x0044 => pub pooling_padding_cfg: ReadWrite<u32>),
        (0x0048 => pub dst_base_addr: ReadWrite<u32>),
        (0x004C => pub dst_surf_stride: ReadWrite<u32>),
        (0x0050 => pub data_format: ReadWrite<u32>),
        (0x0054 => pub misc_ctrl: ReadWrite<u32>),
        (0x0058 => @END),
    }
}
