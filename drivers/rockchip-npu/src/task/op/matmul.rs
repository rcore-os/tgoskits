use super::super::def::*;
use dma_api::{DVec, Direction};

use crate::{
    cna::{NpuCnaDesc, NpuCoreDesc},
    dpu::NpuDpuDesc,
    op::{Operation, OperationTrait, Precision},
};

pub struct MatMul<T: Sized + Copy, O: Sized + Copy> {
    m: u16,
    k: u16,
    n: u16,
    input: DVec<T>,
    weight: DVec<T>,
    output: DVec<O>,
}

impl<T: Sized + Copy, O: Sized + Copy> MatMul<T, O> {
    pub fn new(m: usize, k: usize, n: usize) -> Self {
        Self {
            m: m as _,
            k: k as _,
            n: n as _,
            input: DVec::zeros(
                u32::MAX as _,
                m * k * size_of::<T>(),
                0x1000,
                Direction::Bidirectional,
            )
            .unwrap(),
            weight: DVec::zeros(
                u32::MAX as _,
                k * n * size_of::<T>(),
                0x1000,
                Direction::Bidirectional,
            )
            .unwrap(),
            output: DVec::zeros(
                u32::MAX as _,
                m * n * size_of::<O>(),
                0x1000,
                Direction::Bidirectional,
            )
            .unwrap(),
        }
    }

    pub fn set_a(&mut self, a: &[T]) {
        assert_eq!(a.len(), self.m as usize * self.k as usize);

        let m = self.m as i32;
        let k = self.k as i32;
        for mm in 1..=m {
            for kk in 1..=k {
                let idx = feature_data(k, m, 1, 16, kk, mm, 1) as usize;
                let src = ((mm - 1) * k + (kk - 1)) as usize;
                self.input.set(idx, a[src]);
            }
        }
    }

    fn gen_matul(
        &self,
        reg_cmds: &mut [u64],
        cna: &NpuCnaDesc,
        core: &NpuCoreDesc,
        dpu: &NpuDpuDesc,
    ) {
        // Generate register commands for the matmul operation
        let mut value = 0;

        reg_cmds[0] = npu_op(OP_REG_DPU, value, DPU_S_POINTER); // CNA_DESC_BASE_ADDR

        value = ((cna.proc_precision as u32 & 0x7) << 7)
            | ((cna.in_precision as u32 & 0x7) << 4)
            | (cna.conv_mode as u32 & 0xF);
        reg_cmds[1] = npu_op(OP_REG_CNA, value, CNA_CONV_CON1);
        value =
            ((cna.kernel_groups as u32 & 0xFF) << 16) | ((cna.feature_grains as u32 & 0x3FF) << 4);
        reg_cmds[2] = npu_op(OP_REG_CNA, value, CNA_CONV_CON2);
        value = ((cna.conv_y_stride as u32 & 0x7) << 3) | (cna.conv_x_stride as u32 & 0x7);
        reg_cmds[3] = npu_op(OP_REG_CNA, value, CNA_CONV_CON3);
        value = ((cna.datain_width as u32 & 0x7FF) << 16) | (cna.datain_height as u32 & 0x7FF);
        reg_cmds[4] = npu_op(OP_REG_CNA, value, CNA_DATA_SIZE0);
        value = (((cna.datain_channel - 1) as u32 & 0xFFFF) << 16)
            | (cna.datain_channel as u32 & 0xFFFF);
        reg_cmds[5] = npu_op(OP_REG_CNA, value, CNA_DATA_SIZE1);
        value = cna.dataout_width as u32 & 0x7FF;
        reg_cmds[6] = npu_op(OP_REG_CNA, value, CNA_DATA_SIZE2);
        value = cna.dataout_atomics & 0x3FFFF;
        reg_cmds[7] = npu_op(OP_REG_CNA, value, CNA_DATA_SIZE3);
        value = cna.weight_bytes;
        reg_cmds[8] = npu_op(OP_REG_CNA, value, CNA_WEIGHT_SIZE0);
        value = cna.weight_bytes_per_kernel & 0x7FFFF;
        reg_cmds[9] = npu_op(OP_REG_CNA, value, CNA_WEIGHT_SIZE1);
        value = ((cna.weight_width as u32 & 0x1F) << 24)
            | ((cna.weight_height as u32 & 0x1F) << 16)
            | (cna.weight_kernels as u32 & 0x3FFF);
        reg_cmds[10] = npu_op(OP_REG_CNA, value, CNA_WEIGHT_SIZE2);
        value = ((cna.weight_bank as u32 & 0xF) << 4) | (cna.data_bank as u32 & 0xF);
        reg_cmds[11] = npu_op(OP_REG_CNA, value, CNA_CBUF_CON0);
        value = cna.data_entries as u32 & 0x1FFF;
        reg_cmds[12] = npu_op(OP_REG_CNA, value, CNA_CBUF_CON1);
        value = ((cna.data_sign as u32 & 0x1) << 3)
            | ((cna.cvt_type as u32 & 0x1) << 1)
            | (cna.cvt_bypass as u32 & 0x1);
        reg_cmds[13] = npu_op(OP_REG_CNA, value, CNA_CVT_CON0);
        value = (cna.cvt_scale0 as u32 & 0xFFFF) << 16;
        reg_cmds[14] = npu_op(OP_REG_CNA, value, CNA_CVT_CON1);
        value = (cna.cvt_scale1 as u32 & 0xFFFF) << 16;
        reg_cmds[15] = npu_op(OP_REG_CNA, value, CNA_CVT_CON2);
        value = (cna.cvt_scale2 as u32 & 0xFFFF) << 16;
        reg_cmds[16] = npu_op(OP_REG_CNA, value, CNA_CVT_CON3);
        value = (cna.cvt_scale3 as u32 & 0xFFFF) << 16;
        reg_cmds[17] = npu_op(OP_REG_CNA, value, CNA_CVT_CON4);
        value = cna.fc_skip_en as u32 & 0x1;
        reg_cmds[18] = npu_op(OP_REG_CNA, value, CNA_FC_CON0);
        value = cna.data_offset as u32 & 0x1FFFF;
        reg_cmds[19] = npu_op(OP_REG_CNA, value, CNA_FC_CON1);
        value = ((cna.pad_left as u32 & 0xF) << 4) | (cna.pad_top as u32 & 0xF);
        reg_cmds[20] = npu_op(OP_REG_CNA, value, CNA_PAD_CON0);
        reg_cmds[21] = npu_op(OP_REG_CNA, cna.feature_base_addr, CNA_FEATURE_DATA_ADDR);
        value = cna.weight_offset as u32 & 0x1FFFF;
        reg_cmds[22] = npu_op(OP_REG_CNA, value, CNA_FC_CON2);
        value = ((cna.weight_burst_len as u32 & 0xF) << 16) | (cna.data_burst_len as u32 & 0xF);
        reg_cmds[23] = npu_op(OP_REG_CNA, value, CNA_DMA_CON0);
        value = cna.line_stride & 0xFFFFFFF;
        reg_cmds[24] = npu_op(OP_REG_CNA, value, CNA_DMA_CON1);
        value = cna.surf_stride as u32 & 0xFFFFFFF;
        reg_cmds[25] = npu_op(OP_REG_CNA, value, CNA_DMA_CON2);
        value = ((cna.dma_width as u32 & 0x7FF) << 16) | (cna.dma_height as u32 & 0x7FF);
        reg_cmds[26] = npu_op(OP_REG_CNA, value, CNA_FC_DATA_SIZE0);
        value = cna.dma_channel as u32 & 0xFFFF;
        reg_cmds[27] = npu_op(OP_REG_CNA, value, CNA_FC_DATA_SIZE1);
        reg_cmds[28] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_CTRL);
        reg_cmds[29] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_REGNUM);
        reg_cmds[30] = npu_op(OP_REG_CNA, cna.decompress_addr0, CNA_DCOMP_ADDR0);
        reg_cmds[31] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT);
        reg_cmds[32] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT1);
        reg_cmds[33] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT2);
        reg_cmds[34] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT3);
        reg_cmds[35] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT4);
        reg_cmds[36] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT5);
        reg_cmds[37] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT6);
        reg_cmds[38] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT7);
        reg_cmds[39] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT8);
        reg_cmds[40] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT9);
        reg_cmds[41] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT10);
        reg_cmds[42] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT11);
        reg_cmds[43] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT12);
        reg_cmds[44] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT13);
        reg_cmds[45] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT14);
        reg_cmds[46] = npu_op(OP_REG_CNA, 0x0, CNA_DCOMP_AMOUNT15);
        reg_cmds[47] = npu_op(OP_REG_CNA, 0x0, CNA_CVT_CON5);
        reg_cmds[48] = npu_op(OP_REG_CNA, 0x0, CNA_PAD_CON1);
        value = ((core.proc_precision as u32 & 0x7) << 8) | (core.qd_en as u32 & 0x1);
        reg_cmds[49] = npu_op(OP_REG_CORE, value, CORE_MISC_CFG);
        value =
            ((core.dataout_height as u32 & 0xFFFF) << 16) | (core.dataout_width as u32 & 0xFFFF);
        reg_cmds[50] = npu_op(OP_REG_CORE, value, CORE_DATAOUT_SIZE_0);
        value = core.dataout_channel as u32 & 0xFFFF;
        reg_cmds[51] = npu_op(OP_REG_CORE, value, CORE_DATAOUT_SIZE_1);
        reg_cmds[52] = npu_op(OP_REG_CORE, 0x0, CORE_CLIP_TRUNCATE);
        reg_cmds[53] = npu_op(OP_REG_CORE, 0x0, CORE_3030);
        value = ((dpu.burst_len as u32 & 0xF) << 5)
            | ((dpu.conv_mode as u32 & 0x3) << 3)
            | ((dpu.output_mode as u32 & 0x3) << 1)
            | (dpu.flying_mode as u32 & 0x1);
        reg_cmds[54] = npu_op(OP_REG_DPU, value, DPU_FEATURE_MODE_CFG);
        value = ((dpu.out_precision as u32 & 0x7) << 29)
            | ((dpu.in_precision as u32 & 0x7) << 26)
            | (dpu.proc_precision as u32 & 0x7);
        reg_cmds[55] = npu_op(OP_REG_DPU, value, DPU_DATA_FORMAT);
        reg_cmds[56] = npu_op(OP_REG_DPU, 0x0, DPU_OFFSET_PEND);
        reg_cmds[57] = npu_op(OP_REG_DPU, dpu.dst_base_addr, DPU_DST_BASE_ADD);
        value = (dpu.dst_surf_stride & 0xFFFFFFF) << 4;
        reg_cmds[58] = npu_op(OP_REG_DPU, value, DPU_DST_SURF_STRIDE);
        value = dpu.width as u32 & 0x1FFF;
        reg_cmds[59] = npu_op(OP_REG_DPU, value, DPU_DATA_CUBE_WIDTH);
        value = dpu.height as u32 & 0x1FFF;
        reg_cmds[60] = npu_op(OP_REG_DPU, value, DPU_DATA_CUBE_HEIGHT);
        reg_cmds[61] = npu_op(OP_REG_DPU, 0x0, DPU_DATA_CUBE_NOTCH_ADDR);
        value = ((dpu.channel as u32 & 0x1FFF) << 16) | (dpu.channel as u32 & 0x1FFF);
        reg_cmds[62] = npu_op(OP_REG_DPU, value, DPU_DATA_CUBE_CHANNEL);
        value = ((dpu.bs_relu_bypass as u32 & 0x1) << 6)
            | ((dpu.bs_mul_bypass as u32 & 0x1) << 4)
            | ((dpu.bs_alu_bypass as u32 & 0x1) << 1)
            | (dpu.bs_bypass as u32 & 0x1);
        reg_cmds[63] = npu_op(OP_REG_DPU, value, DPU_BS_CFG);
        reg_cmds[64] = npu_op(OP_REG_DPU, 0x0, DPU_BS_ALU_CFG);
        reg_cmds[65] = npu_op(OP_REG_DPU, 0x0, DPU_BS_MUL_CFG);
        reg_cmds[66] = npu_op(OP_REG_DPU, 0x0, DPU_BS_RELUX_CMP_VALUE);
        value = ((dpu.size_e_2 as u32 & 0x7) << 8)
            | ((dpu.size_e_1 as u32 & 0x7) << 5)
            | ((dpu.size_e_0 as u32 & 0x7) << 2)
            | ((dpu.od_bypass as u32 & 0x1) << 1);
        reg_cmds[67] = npu_op(OP_REG_DPU, value, DPU_BS_OW_CFG);
        reg_cmds[68] = npu_op(OP_REG_DPU, 0x0, DPU_BS_OW_OP);
        value = dpu.channel_wdma as u32 & 0x1FFF;
        reg_cmds[69] = npu_op(OP_REG_DPU, value, DPU_WDMA_SIZE_0);
        value = ((dpu.height_wdma as u32 & 0x1FFF) << 16) | (dpu.width_wdma as u32 & 0x1FFF);
        reg_cmds[70] = npu_op(OP_REG_DPU, value, DPU_WDMA_SIZE_1);
        value = ((dpu.bn_relu_bypass as u32 & 0x1) << 6)
            | ((dpu.bn_mul_bypass as u32 & 0x1) << 4)
            | ((dpu.bn_alu_bypass as u32 & 0x1) << 1)
            | (dpu.bn_bypass as u32 & 0x1);
        reg_cmds[71] = npu_op(OP_REG_DPU, value, DPU_BN_CFG);
        reg_cmds[72] = npu_op(OP_REG_DPU, 0x0, DPU_BN_ALU_CFG);
        reg_cmds[73] = npu_op(OP_REG_DPU, 0x0, DPU_BN_MUL_CFG);
        reg_cmds[74] = npu_op(OP_REG_DPU, 0x0, DPU_BN_RELUX_CMP_VALUE);
        value = ((dpu.ew_relu_bypass as u32 & 0x1) << 9)
            | ((dpu.ew_op_cvt_bypass as u32 & 0x1) << 8)
            | ((dpu.ew_lut_bypass as u32 & 0x1) << 7)
            | ((dpu.ew_op_bypass as u32 & 0x1) << 1)
            | (dpu.ew_bypass as u32 & 0x1);
        reg_cmds[75] = npu_op(OP_REG_DPU, value, DPU_EW_CFG);
        reg_cmds[76] = npu_op(OP_REG_DPU, 0x0, DPU_EW_CVT_OFFSET_VALUE);
        reg_cmds[77] = npu_op(OP_REG_DPU, 0x1, DPU_EW_CVT_SCALE_VALUE);
        reg_cmds[78] = npu_op(OP_REG_DPU, 0x0, DPU_EW_RELUX_CMP_VALUE);
        reg_cmds[79] = npu_op(OP_REG_DPU, 0x0, DPU_OUT_CVT_OFFSET);
        value = ((dpu.fp32tofp16_en as u32 & 0x1) << 16) | (dpu.out_cvt_scale as u32 & 0xFFFF);
        reg_cmds[80] = npu_op(OP_REG_DPU, value, DPU_OUT_CVT_SCALE);
        reg_cmds[81] = npu_op(OP_REG_DPU, 0x0, DPU_OUT_CVT_SHIFT);
        reg_cmds[82] = npu_op(OP_REG_DPU, 0x0, DPU_EW_OP_VALUE_0);
        reg_cmds[83] = npu_op(OP_REG_DPU, 0x0, DPU_EW_OP_VALUE_1);
        reg_cmds[84] = npu_op(OP_REG_DPU, 0x0, DPU_EW_OP_VALUE_2);
        reg_cmds[85] = npu_op(OP_REG_DPU, 0x0, DPU_EW_OP_VALUE_3);
        reg_cmds[86] = npu_op(OP_REG_DPU, 0x0, DPU_EW_OP_VALUE_4);
        reg_cmds[87] = npu_op(OP_REG_DPU, 0x0, DPU_EW_OP_VALUE_5);
        reg_cmds[88] = npu_op(OP_REG_DPU, 0x0, DPU_EW_OP_VALUE_6);
        reg_cmds[89] = npu_op(OP_REG_DPU, 0x0, DPU_EW_OP_VALUE_7);
        value = (dpu.surf_add & 0xFFFFFFF) << 4;
        reg_cmds[90] = npu_op(OP_REG_DPU, value, DPU_SURFACE_ADD);
        reg_cmds[91] = npu_op(OP_REG_DPU, 0x0, DPU_40C4);
        reg_cmds[92] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_ACCESS_CFG);
        reg_cmds[93] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_ACCESS_DATA);
        reg_cmds[94] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_CFG);
        reg_cmds[95] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_INFO);
        reg_cmds[96] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_LE_START);
        reg_cmds[97] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_LE_END);
        reg_cmds[98] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_LO_START);
        reg_cmds[99] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_LO_END);
        reg_cmds[100] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_LE_SLOPE_SCALE);
        reg_cmds[101] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_LE_SLOPE_SHIFT);
        reg_cmds[102] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_LO_SLOPE_SCALE);
        reg_cmds[103] = npu_op(OP_REG_DPU, 0x0, DPU_LUT_LO_SLOPE_SHIFT);
        reg_cmds[104] = npu_op(OP_NONE, 0x0, 0x0);
        reg_cmds[105] = npu_op(OP_REG_PC, 0x0, PC_REGISTER_AMOUNTS);
        reg_cmds[106] = npu_op(OP_40, 0x0, 0x0);
        reg_cmds[107] = npu_op(
            OP_ENABLE,
            PC_ENABLE_DPU | PC_ENABLE_CNA | PC_ENABLE,
            PC_OPERATION_ENABLE,
        );
    }
}

impl OperationTrait for MatMul<i8, i32> {
    fn fill_regcmd(&self, regcmd: &mut [u64]) {
        let mut cna_desc = NpuCnaDesc::default();
        let mut core_desc = NpuCoreDesc::default();
        let mut dpu_desc = NpuDpuDesc::default();

        debug!(
            "Generating matmul task: M={}, K={}, N={}",
            self.m, self.k, self.n
        );
        debug!("Input feature address: {:#x}", self.input.bus_addr());
        debug!("Weight address: {:#x}", self.weight.bus_addr());
        debug!("Output address: {:#x}", self.output.bus_addr());

        cna_desc.conv_mode = DIRECT_CONVOLUTION;
        cna_desc.in_precision = Precision::Int8 as u8;
        cna_desc.proc_precision = Precision::Int8 as u8;

        cna_desc.kernel_groups = 0;
        cna_desc.feature_grains = self.m + 1;
        cna_desc.conv_x_stride = 1;
        cna_desc.conv_y_stride = 1;

        cna_desc.datain_width = 1;
        cna_desc.datain_height = self.m;
        cna_desc.datain_channel = self.k;
        cna_desc.dataout_width = 1;
        cna_desc.dataout_height = self.m;
        cna_desc.dataout_atomics = cna_desc.dataout_width as u32 * cna_desc.dataout_height as u32;

        cna_desc.weight_width = 1;
        cna_desc.weight_height = 1;
        cna_desc.weight_kernels = self.n;
        cna_desc.weight_bytes_per_kernel = cna_desc.weight_width as u32
            * cna_desc.weight_height as u32
            * cna_desc.datain_channel as u32
            * size_of::<u8>() as u32;
        cna_desc.weight_bytes = cna_desc.weight_bytes_per_kernel * cna_desc.weight_kernels as u32;

        let fd_bytes = cna_desc.datain_width
            * cna_desc.datain_height
            * cna_desc.datain_channel
            * size_of::<u8>() as u16;
        let mut fd_banks = fd_bytes / NPU_CBUF_BANK_SIZE;
        fd_banks = if fd_bytes.is_multiple_of(NPU_CBUF_BANK_SIZE) {
            fd_banks
        } else {
            fd_banks + 1
        };
        let mut weight_banks = cna_desc.weight_bytes / NPU_CBUF_BANK_SIZE as u32;
        weight_banks = if (cna_desc.weight_bytes % NPU_CBUF_BANK_SIZE as u32) == 0 {
            weight_banks
        } else {
            weight_banks + 1
        };
        if (fd_banks) > NPU_CBUF_BANKS - 1 {
            panic!("Input feature data size exceed cbuf size");
        } else if cna_desc.weight_bytes_per_kernel <= NPU_CBUF_BANK_SIZE as u32 {
            weight_banks = NPU_CBUF_BANKS as u32 - fd_banks as u32;
        } else {
            panic!("Weight data size exceed cbuf size");
        }

        cna_desc.weight_bank = weight_banks as _;
        cna_desc.data_bank = fd_banks as _;
        cna_desc.data_entries = (cna_desc.datain_width * cna_desc.datain_channel) / 64;
        cna_desc.data_entries = if (cna_desc.datain_width * cna_desc.datain_channel) % 64 == 0 {
            cna_desc.data_entries
        } else {
            cna_desc.data_entries + 1
        };
        cna_desc.data_sign = 0x1;
        cna_desc.cvt_type = 0x1;
        cna_desc.cvt_bypass = 0x1;
        cna_desc.cvt_scale0 = 0x1;
        cna_desc.cvt_scale1 = 0x1;
        cna_desc.cvt_scale2 = 0x1;
        cna_desc.cvt_scale3 = 0x1;
        cna_desc.fc_skip_en = 0;
        cna_desc.data_offset = 0x0;
        cna_desc.pad_left = 0;
        cna_desc.pad_top = 0;
        cna_desc.feature_base_addr = self.input.bus_addr() as u32;
        cna_desc.weight_offset = 0;
        cna_desc.weight_burst_len = 0xf;
        cna_desc.data_burst_len = 0xf;
        cna_desc.line_stride = cna_desc.datain_width as u32 * 4;
        let mut surf_stride =
            cna_desc.line_stride as i32 * ((cna_desc.datain_height as i32 / 4) - 1);
        surf_stride = if surf_stride < 0 {
            surf_stride + 1
        } else {
            surf_stride
        };
        cna_desc.surf_stride = surf_stride;
        cna_desc.dma_width = cna_desc.datain_width;
        cna_desc.dma_height = cna_desc.datain_height;
        cna_desc.dma_channel = cna_desc.datain_channel;
        cna_desc.decompress_addr0 = self.weight.bus_addr() as _;

        core_desc.proc_precision = Precision::Int8 as u8;
        core_desc.qd_en = 0;
        core_desc.dataout_height = cna_desc.dataout_height - 1;
        core_desc.dataout_width = cna_desc.dataout_width - 1;
        core_desc.dataout_channel = cna_desc.weight_kernels - 1;

        dpu_desc.burst_len = 0xf;
        dpu_desc.conv_mode = DIRECT_CONVOLUTION;
        dpu_desc.output_mode = 0x2;
        dpu_desc.flying_mode = 0x0;
        dpu_desc.out_precision = Precision::Int32 as u8;
        dpu_desc.in_precision = Precision::Int8 as u8;
        dpu_desc.proc_precision = Precision::Int8 as u8;
        dpu_desc.dst_base_addr = self.output.bus_addr() as _;
        dpu_desc.dst_surf_stride = cna_desc.dataout_height as u32 * cna_desc.dataout_width as u32;
        dpu_desc.width = core_desc.dataout_width;
        dpu_desc.height = core_desc.dataout_height;
        dpu_desc.channel = core_desc.dataout_channel;
        dpu_desc.bs_bypass = 1;
        dpu_desc.bs_alu_bypass = 1;
        dpu_desc.bs_mul_bypass = 1;
        dpu_desc.bs_relu_bypass = 1;
        dpu_desc.bn_bypass = 1;
        dpu_desc.bn_alu_bypass = 1;
        dpu_desc.bn_mul_bypass = 1;
        dpu_desc.bn_relu_bypass = 1;
        dpu_desc.ew_bypass = 1;
        dpu_desc.ew_op_bypass = 1;
        dpu_desc.ew_lut_bypass = 1;
        dpu_desc.ew_op_cvt_bypass = 1;
        dpu_desc.ew_relu_bypass = 1;
        dpu_desc.fp32tofp16_en = 0;
        dpu_desc.out_cvt_scale = 1;
        dpu_desc.size_e_2 = 7;
        dpu_desc.size_e_1 = 7;
        dpu_desc.size_e_0 = 7;
        dpu_desc.od_bypass = 1;
        dpu_desc.width_wdma = core_desc.dataout_width;
        dpu_desc.height_wdma = core_desc.dataout_height;
        dpu_desc.channel_wdma = core_desc.dataout_channel;
        dpu_desc.surf_add = dpu_desc.dst_surf_stride * 8;

        self.gen_matul(regcmd, &cna_desc, &core_desc, &dpu_desc);
    }
}

impl MatMul<i8, i32> {
    pub fn new_i8(m: usize, k: usize, n: usize) -> Operation {
        Operation::MatMulu8(MatMul::new(m, k, n))
    }

    pub fn set_b(&mut self, b: &[i8]) {
        assert_eq!(b.len(), self.k as usize * self.n as usize);

        let k = self.k as i32;
        let n = self.n as i32;
        for nn in 1..=n {
            for kk in 1..=k {
                let idx = weight_int8(k, nn, kk) as usize;
                let src = ((nn - 1) * k + (kk - 1)) as usize;
                self.weight.set(idx, b[src]);
            }
        }
    }

    pub fn get_output(&self, m: usize, n: usize) -> i32 {
        self.output
            .get(feature_data(self.n as _, self.m as _, 1, 4, n as _, m as _, 1) as usize)
            .unwrap()
    }

    pub fn output_buffer(&self) -> &[i32] {
        self.output.as_ref()
    }
}

fn feature_data(
    _channel: i32,
    height: i32,
    width: i32,
    channel_group: i32,
    c: i32,
    h: i32,
    w: i32,
) -> i32 {
    let plane = (c - 1) / channel_group;
    let src_offset = plane * height * width * channel_group;
    let channel_offset = (c - 1) % channel_group;

    src_offset + channel_group * ((h - 1) * width + (w - 1)) + channel_offset
}

fn weight_int8(channel: i32, kernel: i32, c: i32) -> i32 {
    let kernel_page = (kernel - 1) / 32;
    let channel_page = (c - 1) / 32;
    let mut dst = (channel_page * 32) * 32 + (kernel_page * 32 * channel);
    dst += (c - 1) % 32 + ((kernel - 1) % 32) * 32;
    dst
}
