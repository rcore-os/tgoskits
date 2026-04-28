#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NpuDpuDesc {
    pub burst_len: u8,        // 0x400C
    pub conv_mode: u8,        // 0x400C
    pub output_mode: u8,      // 0x400C
    pub flying_mode: u8,      // 0x400C
    pub out_precision: u8,    // 0x4010
    pub in_precision: u8,     // 0x4010
    pub proc_precision: u8,   // 0x4010
    pub dst_base_addr: u32,   // 0x4020
    pub dst_surf_stride: u32, // 0x4024
    pub width: u16,           // 0x4030
    pub height: u16,          // 0x4034
    pub channel: u16,         // 0x403C
    pub bs_bypass: u8,        // 0x4040
    pub bs_alu_bypass: u8,    // 0x4040
    pub bs_mul_bypass: u8,    // 0x4040
    pub bs_relu_bypass: u8,   // 0x4040
    pub od_bypass: u8,        // 0x4050
    pub size_e_2: u8,         // 0x4050
    pub size_e_1: u8,         // 0x4050
    pub size_e_0: u8,         // 0x4050
    pub channel_wdma: u16,    // 0x4058
    pub height_wdma: u16,     // 0x405C
    pub width_wdma: u16,      // 0x405C
    pub bn_relu_bypass: u8,   // 0x4060
    pub bn_mul_bypass: u8,    // 0x4060
    pub bn_alu_bypass: u8,    // 0x4060
    pub bn_bypass: u8,        // 0x4060
    pub ew_bypass: u8,        // 0x4070
    pub ew_op_bypass: u8,     // 0x4070
    pub ew_lut_bypass: u8,    // 0x4070
    pub ew_op_cvt_bypass: u8, // 0x4070
    pub ew_relu_bypass: u8,   // 0x4070
    pub fp32tofp16_en: u8,    // 0x4084
    pub out_cvt_scale: u16,   // 0x4084
    pub surf_add: u32,        // 0x40C0
}
