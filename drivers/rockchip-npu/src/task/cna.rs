#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NpuCnaDesc {
    pub enable: u8,
    pub conv_mode: u8,                // 0x100C
    pub in_precision: u8,             // 0x100C
    pub proc_precision: u8,           // 0x100C
    pub kernel_groups: u8,            // 0x1010
    pub feature_grains: u16,          // 0x1010
    pub conv_y_stride: u8,            // 0x1014
    pub conv_x_stride: u8,            // 0x1014
    pub datain_width: u16,            // 0x1020
    pub datain_height: u16,           // 0x1020
    pub datain_channel: u16,          // 0x1024
    pub dataout_width: u16,           // 0x1028
    pub dataout_atomics: u32,         // 0x102C
    pub weight_bytes: u32,            // 0x1030
    pub weight_bytes_per_kernel: u32, // 0x1034
    pub weight_width: u8,             // 0x1038
    pub weight_height: u8,            // 0x1038
    pub weight_kernels: u16,          // 0x1038
    pub weight_bank: u8,              // 0x1040
    pub data_bank: u8,                // 0x1040
    pub data_entries: u16,            // 0x1044
    pub data_sign: u8,                // 0x104c
    pub cvt_type: u8,                 // 0x104c
    pub cvt_bypass: u8,               // 0x104c
    pub cvt_scale0: u16,              // 0x1050
    pub cvt_scale1: u16,              // 0x1054
    pub cvt_scale2: u16,              // 0x1058
    pub cvt_scale3: u16,              // 0x105C
    pub fc_skip_en: u8,               // 0x1060
    pub data_offset: u16,             // 0x1064
    pub pad_left: u8,                 // 0x1068
    pub pad_top: u8,                  // 0x1068
    pub feature_base_addr: u32,       // 0x1070
    pub weight_offset: u16,           // 0x1074
    pub weight_burst_len: u8,         // 0x1078
    pub data_burst_len: u8,           // 0x1078
    pub line_stride: u32,             // 0x107C
    pub surf_stride: i32,             // 0x1080
    pub dma_width: u16,               // 0x1084
    pub dma_height: u16,              // 0x1084
    pub dma_channel: u16,             // 0x1088
    pub decompress_addr0: u32,        // 0x1110
    pub dataout_height: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NpuCoreDesc {
    pub proc_precision: u8,   // 0x3010
    pub qd_en: u8,            // 0x3010
    pub dataout_height: u16,  // 0x3014
    pub dataout_width: u16,   // 0x3014
    pub dataout_channel: u16, // 0x3018
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NpuPcDesc {
    pub pc_source_addr: u32, // 0x0010
    pub pc_data_amount: u32, // 0x0014
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NpuCnaCoreTask {
    pub ops: [u64; 112],
}
