#![allow(dead_code)]

pub const DIRECT_CONVOLUTION: u8 = 0x0;
pub const NPU_CBUF_BANK_SIZE: u16 = 32768;
pub const NPU_CBUF_BANKS: u16 = 12;

pub const fn npu_op(op: u32, value: u32, reg: u32) -> u64 {
    ((op as u64 & 0xFFFF) << 48) | ((value as u64 & 0xFFFF_FFFF) << 16) | reg as u64
}

// Registers as per TRM V1.0 2022-03-09 and descriptions (can be cryptic or missing)
pub const PC_OPERATION_ENABLE: u32 = 0x0008; // Operation Enable
pub const PC_BASE_ADDRESS: u32 = 0x0010; // PC address register
pub const PC_REGISTER_AMOUNTS: u32 = 0x0014; // Register amount for each task

pub const CNA_S_POINTER: u32 = 0x1004; // Single register group pointer
pub const CNA_CONV_CON1: u32 = 0x100C; // Convolution control register1
pub const CNA_CONV_CON2: u32 = 0x1010; // Convolution control register2
pub const CNA_CONV_CON3: u32 = 0x1014; // Convolution control register3
pub const CNA_DATA_SIZE0: u32 = 0x1020; // Feature data size control register0
pub const CNA_DATA_SIZE1: u32 = 0x1024; // Feature data size control register1
pub const CNA_DATA_SIZE2: u32 = 0x1028; // Feature data size control register2
pub const CNA_DATA_SIZE3: u32 = 0x102C; // Feature data size control register3
pub const CNA_WEIGHT_SIZE0: u32 = 0x1030; // Weight size control 0
pub const CNA_WEIGHT_SIZE1: u32 = 0x1034; // Weight size control 1
pub const CNA_WEIGHT_SIZE2: u32 = 0x1038; // Weight size control 2
pub const CNA_CBUF_CON0: u32 = 0x1040; // CBUF control register 0
pub const CNA_CBUF_CON1: u32 = 0x1044; // CBUF control register 1
pub const CNA_CVT_CON0: u32 = 0x104C; // Input convert control register0
pub const CNA_CVT_CON1: u32 = 0x1050; // Input convert control register1
pub const CNA_CVT_CON2: u32 = 0x1054; // Input convert control register2
pub const CNA_CVT_CON3: u32 = 0x1058; // Input convert control register3
pub const CNA_CVT_CON4: u32 = 0x105C; // Input convert control register4
pub const CNA_FC_CON0: u32 = 0x1060; // Full connected control register0
pub const CNA_FC_CON1: u32 = 0x1064; // Full connected control register1
pub const CNA_PAD_CON0: u32 = 0x1068; // Pad control register0
pub const CNA_FEATURE_DATA_ADDR: u32 = 0x1070; // Base address for input feature data
pub const CNA_FC_CON2: u32 = 0x1074; // Full connected control register2
pub const CNA_DMA_CON0: u32 = 0x1078; // AXI control register 0
pub const CNA_DMA_CON1: u32 = 0x107C; // AXI control register 1
pub const CNA_DMA_CON2: u32 = 0x1080; // AXI control register 2
pub const CNA_FC_DATA_SIZE0: u32 = 0x1084; // Full connected data size control register0
pub const CNA_FC_DATA_SIZE1: u32 = 0x1088; // Full connected data size control register1
pub const CNA_DCOMP_CTRL: u32 = 0x1100; // Weight decompress control register
pub const CNA_DCOMP_REGNUM: u32 = 0x1104; // Weight decompress register number
pub const CNA_DCOMP_ADDR0: u32 = 0x1110; // Base address of the weight
pub const CNA_DCOMP_AMOUNT: u32 = 0x1140; // Amount of the weight decompress for the 0 decompress
pub const CNA_DCOMP_AMOUNT1: u32 = 0x1144; // Amount of the weight decompress for the 1 decompress
pub const CNA_DCOMP_AMOUNT2: u32 = 0x1148; // Amount of the weight decompress for the 2 decompress
pub const CNA_DCOMP_AMOUNT3: u32 = 0x114C; // Amount of the weight decompress for the 3 decompress
pub const CNA_DCOMP_AMOUNT4: u32 = 0x1150; // Amount of the weight decompress for the 4 decompress
pub const CNA_DCOMP_AMOUNT5: u32 = 0x1154; // Amount of the weight decompress for the 5 decompress
pub const CNA_DCOMP_AMOUNT6: u32 = 0x1158; // Amount of the weight decompress for the 6 decompress
pub const CNA_DCOMP_AMOUNT7: u32 = 0x115C; // Amount of the weight decompress for the 7 decompress
pub const CNA_DCOMP_AMOUNT8: u32 = 0x1160; // Amount of the weight decompress for the 8 decompress
pub const CNA_DCOMP_AMOUNT9: u32 = 0x1164; // Amount of the weight decompress for the 9 decompress
pub const CNA_DCOMP_AMOUNT10: u32 = 0x1168; // Amount of the weight decompress for the 10 decompress
pub const CNA_DCOMP_AMOUNT11: u32 = 0x116C; // Amount of the weight decompress for the 11 decompress
pub const CNA_DCOMP_AMOUNT12: u32 = 0x1170; // Amount of the weight decompress for the 12 decompress
pub const CNA_DCOMP_AMOUNT13: u32 = 0x1174; // Amount of the weight decompress for the 13 decompress
pub const CNA_DCOMP_AMOUNT14: u32 = 0x1178; // Amount of the weight decompress for the 14 decompress
pub const CNA_DCOMP_AMOUNT15: u32 = 0x117C; // Amount of the weight decompress for the 15 decompress
pub const CNA_CVT_CON5: u32 = 0x1180; // Input convert control register5
pub const CNA_PAD_CON1: u32 = 0x1184; // Pad controller register1

pub const CORE_S_POINTER: u32 = 0x3004; // Single register group pointer
pub const CORE_MISC_CFG: u32 = 0x3010; // Misc configuration register
pub const CORE_DATAOUT_SIZE_0: u32 = 0x3014; // Feature size register 0 of output
pub const CORE_DATAOUT_SIZE_1: u32 = 0x3018; // Feature size register 1 of output
pub const CORE_CLIP_TRUNCATE: u32 = 0x301C; // Shift value register
pub const CORE_3030: u32 = 0x3030; // Doesn't seem to be documented, is it required ??

pub const DPU_S_POINTER: u32 = 0x4004; // Single register group pointer
pub const DPU_FEATURE_MODE_CFG: u32 = 0x400C; // Configuration of the feature mode
pub const DPU_DATA_FORMAT: u32 = 0x4010; // Configuration of the data format
pub const DPU_OFFSET_PEND: u32 = 0x4014; // Value of the offset pend
pub const DPU_DST_BASE_ADD: u32 = 0x4020; // Destination base address
pub const DPU_DST_SURF_STRIDE: u32 = 0x4024; // Destination surface size
pub const DPU_DATA_CUBE_WIDTH: u32 = 0x4030; // Width of the input cube
pub const DPU_DATA_CUBE_HEIGHT: u32 = 0x4034; // Height of the input cube  
pub const DPU_DATA_CUBE_NOTCH_ADDR: u32 = 0x4038; // Notch signal of the input cube
pub const DPU_DATA_CUBE_CHANNEL: u32 = 0x403C; // Channel of the input cube
pub const DPU_BS_CFG: u32 = 0x4040; // Configuration of the BS
pub const DPU_BS_ALU_CFG: u32 = 0x4044; // Configuration of the BS ALU
pub const DPU_BS_MUL_CFG: u32 = 0x4048; // Configuration of the BS MUL
pub const DPU_BS_RELUX_CMP_VALUE: u32 = 0x404C; // Value of the RELUX compare with
pub const DPU_BS_OW_CFG: u32 = 0x4050; // Configuration of the BS OW
pub const DPU_BS_OW_OP: u32 = 0x4054; // Ow op of the BS OW
pub const DPU_WDMA_SIZE_0: u32 = 0x4058; // Size 0 of the WDMA
pub const DPU_WDMA_SIZE_1: u32 = 0x405C; // Size 1 of the WDMA
pub const DPU_BN_CFG: u32 = 0x4060; // Configuration of BN
pub const DPU_BN_ALU_CFG: u32 = 0x4064; // Configuration of the BN ALU
pub const DPU_BN_MUL_CFG: u32 = 0x4068; // Configuration of the BN MUL
pub const DPU_BN_RELUX_CMP_VALUE: u32 = 0x406C; // Value of the RELUX compare with
pub const DPU_EW_CFG: u32 = 0x4070; // Configuration of EW
pub const DPU_EW_CVT_OFFSET_VALUE: u32 = 0x4074; // Offset of the EW input convert
pub const DPU_EW_CVT_SCALE_VALUE: u32 = 0x4078; // Scale of the EW input convert
pub const DPU_EW_RELUX_CMP_VALUE: u32 = 0x407C; // Value of the RELUX compare with
pub const DPU_OUT_CVT_OFFSET: u32 = 0x4080; // Offset of the output converter
pub const DPU_OUT_CVT_SCALE: u32 = 0x4084; // Scale of the output converter
pub const DPU_OUT_CVT_SHIFT: u32 = 0x4088; // Shift of the output converter
pub const DPU_EW_OP_VALUE_0: u32 = 0x4090; // Configure operand0 of the EW
pub const DPU_EW_OP_VALUE_1: u32 = 0x4094; // Configure operand1 of the EW
pub const DPU_EW_OP_VALUE_2: u32 = 0x4098; // Configure operand2 of the EW
pub const DPU_EW_OP_VALUE_3: u32 = 0x409C; // Configure operand3 of the EW
pub const DPU_EW_OP_VALUE_4: u32 = 0x40A0; // Configure operand4 of the EW
pub const DPU_EW_OP_VALUE_5: u32 = 0x40A4; // Configure operand5 of the EW
pub const DPU_EW_OP_VALUE_6: u32 = 0x40A8; // Configure operand6 of the EW
pub const DPU_EW_OP_VALUE_7: u32 = 0x40AC; // Configure operand7 of the EW
pub const DPU_SURFACE_ADD: u32 = 0x40C0; // Value of the surface adder
pub const DPU_40C4: u32 = 0x40C4; // Not documented      
pub const DPU_LUT_ACCESS_CFG: u32 = 0x4100; // LUT access address and type
pub const DPU_LUT_ACCESS_DATA: u32 = 0x4104; // Configuration of LUT access data
pub const DPU_LUT_CFG: u32 = 0x4108; // Configuration of the LUT
pub const DPU_LUT_INFO: u32 = 0x410C; // LUT information register
pub const DPU_LUT_LE_START: u32 = 0x4110; // LE LUT start point
pub const DPU_LUT_LE_END: u32 = 0x4114; // LE LUT end point
pub const DPU_LUT_LO_START: u32 = 0x4118; // LO LUT start point
pub const DPU_LUT_LO_END: u32 = 0x411C; // LO LUT end point
pub const DPU_LUT_LE_SLOPE_SCALE: u32 = 0x4120; // LE LUT slope scale
pub const DPU_LUT_LE_SLOPE_SHIFT: u32 = 0x4124; // LE LUT slope shift
pub const DPU_LUT_LO_SLOPE_SCALE: u32 = 0x4128; // LO LUT slope scale
pub const DPU_LUT_LO_SLOPE_SHIFT: u32 = 0x412C; // LO LUT slope shift

// NPU capability is limited to the following units
pub const BLOCK_PC: u32 = 0x0100;
pub const BLOCK_CNA: u32 = 0x0200;
pub const BLOCK_CORE: u32 = 0x0800;
pub const BLOCK_DPU: u32 = 0x1000;
pub const BLOCK_DPU_RDMA: u32 = 0x2000;
pub const BLOCK_PPU: u32 = 0x4000;
pub const BLOCK_PPU_RDMA: u32 = 0x8000;

pub const PC_OP_01: u32 = 0x01; // reg ??
pub const PC_OP_40: u32 = 0x40; // ??
pub const PC_OP_ENABLE: u32 = 0x80; // Enables block(s)

pub const OP_REG_PC: u32 = BLOCK_PC | PC_OP_01; // ??
pub const OP_REG_CNA: u32 = BLOCK_CNA | PC_OP_01; // ??
pub const OP_REG_CORE: u32 = BLOCK_CORE | PC_OP_01; // ??
pub const OP_REG_DPU: u32 = BLOCK_DPU | PC_OP_01; // ??

pub const OP_40: u32 = PC_OP_40 | PC_OP_01; // ??
pub const OP_ENABLE: u32 = PC_OP_ENABLE | PC_OP_01; // ??
pub const OP_NONE: u32 = 0x0; // ??

pub const PC_ENABLE: u32 = 0x01; // Enable for this task
pub const PC_ENABLE_CNA: u32 = 0x04; // ?? Interrupt
pub const PC_ENABLE_DPU: u32 = 0x08; // ?? Interrupt
pub const PC_ENABLE_PPU: u32 = 0x10; // ?? Interrupt
