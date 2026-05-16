//! TPU 数据结构定义

// ============================================================================
// IOCTL 命令定义
// ============================================================================

/// IOCTL 基础命令号
pub const IOCTL_TPU_BASE: u8 = b'p';

/// 计算 _IOW 命令码 (写操作)
const fn iow(ty: u8, nr: u8, size: usize) -> u32 {
    // _IOW: direction = 1 (write)
    (1u32 << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | (nr as u32)
}

/// 计算 _IOWR 命令码 (读写操作)
const fn iowr(ty: u8, nr: u8, size: usize) -> u32 {
    // _IOWR: direction = 3 (read | write)
    (3u32 << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | (nr as u32)
}

/// 提交 DMA buffer 执行
pub const CVITPU_SUBMIT_DMABUF: u32 = iow(IOCTL_TPU_BASE, 0x01, 8);
/// 刷新 DMA buffer (通过 fd)
pub const CVITPU_DMABUF_FLUSH_FD: u32 = iow(IOCTL_TPU_BASE, 0x02, 8);
/// 无效化 DMA buffer (通过 fd)
pub const CVITPU_DMABUF_INVLD_FD: u32 = iow(IOCTL_TPU_BASE, 0x03, 8);
/// 刷新 DMA buffer (通过物理地址)
pub const CVITPU_DMABUF_FLUSH: u32 = iow(IOCTL_TPU_BASE, 0x04, 8);
/// 无效化 DMA buffer (通过物理地址)
pub const CVITPU_DMABUF_INVLD: u32 = iow(IOCTL_TPU_BASE, 0x05, 8);
/// 等待 DMA buffer 完成
pub const CVITPU_WAIT_DMABUF: u32 = iowr(IOCTL_TPU_BASE, 0x06, 8);
/// PIO 模式执行
pub const CVITPU_PIO_MODE: u32 = iow(IOCTL_TPU_BASE, 0x07, 8);
/// 加载 TEE
pub const CVITPU_LOAD_TEE: u32 = iowr(IOCTL_TPU_BASE, 0x08, 8);
/// 提交 TEE
pub const CVITPU_SUBMIT_TEE: u32 = iow(IOCTL_TPU_BASE, 0x09, 8);
/// 卸载 TEE
pub const CVITPU_UNLOAD_TEE: u32 = iow(IOCTL_TPU_BASE, 0x0A, 8);
/// 提交 PIO
pub const CVITPU_SUBMIT_PIO: u32 = iow(IOCTL_TPU_BASE, 0x0B, 8);
/// 等待 PIO
pub const CVITPU_WAIT_PIO: u32 = iowr(IOCTL_TPU_BASE, 0x0C, 8);

// ============================================================================
// IOCTL 数据结构
// ============================================================================

/// 缓存操作参数
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CviCacheOpArg {
    /// 物理地址
    pub paddr: u64,
    /// 大小
    pub size: u64,
    /// DMA 文件描述符
    pub dma_fd: i32,
    /// 填充对齐
    pub _padding: i32,
}

/// 提交 DMA 参数
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CviSubmitDmaArg {
    /// DMA buffer 文件描述符
    pub fd: i32,
    /// 序列号
    pub seq_no: u32,
}

/// 等待 DMA 参数
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CviWaitDmaArg {
    /// 序列号
    pub seq_no: u32,
    /// 返回值
    pub ret: i32,
}

/// PIO 模式参数
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CviPioMode {
    /// 命令缓冲区地址
    pub cmdbuf: u64,
    /// 大小
    pub sz: u64,
}

/// 加载 TEE 参数
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CviLoadTeeArg {
    /// REE 域命令缓冲区地址
    pub cmdbuf_addr_ree: u64,
    /// REE 域命令缓冲区长度
    pub cmdbuf_len_ree: u32,
    /// 填充
    pub _pad1: u32,
    /// REE 域权重地址
    pub weight_addr_ree: u64,
    /// REE 域权重长度
    pub weight_len_ree: u32,
    /// 填充
    pub _pad2: u32,
    /// REE 域神经元地址
    pub neuron_addr_ree: u64,
    /// TEE 域 DMA buffer 地址
    pub dmabuf_addr_tee: u64,
}

/// 提交 TEE 参数
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CviSubmitTeeArg {
    /// TEE DMA buffer 地址
    pub dmabuf_tee_addr: u64,
    /// 全局地址 base 2
    pub gaddr_base2: u64,
    /// 全局地址 base 3
    pub gaddr_base3: u64,
    /// 全局地址 base 4
    pub gaddr_base4: u64,
    /// 全局地址 base 5
    pub gaddr_base5: u64,
    /// 全局地址 base 6
    pub gaddr_base6: u64,
    /// 全局地址 base 7
    pub gaddr_base7: u64,
    /// 序列号
    pub seq_no: u32,
    /// 填充
    pub _padding: u32,
}

/// 卸载 TEE 参数
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CviUnloadTeeArg {
    /// 地址
    pub addr: u64,
    /// 大小
    pub size: u64,
}

/// TDMA 复制参数
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CviTdmaCopyArg {
    /// 源物理地址
    pub paddr_src: u64,
    /// 目标物理地址
    pub paddr_dst: u64,
    /// 高度
    pub h: u32,
    /// 宽度 (字节)
    pub w_bytes: u32,
    /// 源步长 (字节)
    pub stride_bytes_src: u32,
    /// 目标步长 (字节)
    pub stride_bytes_dst: u32,
    /// 启用 2D
    pub enable_2d: u32,
    /// 长度 (字节)
    pub leng_bytes: u32,
    /// 序列号
    pub seq_no: u32,
    /// 填充
    pub _padding: u32,
}

/// TDMA 等待参数
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CviTdmaWaitArg {
    /// 序列号
    pub seq_no: u32,
    /// 返回值
    pub ret: i32,
}

// ============================================================================
// DMA buffer 头部结构
// ============================================================================

/// DMA buffer 头部结构
/// 对应 C 结构体 dma_hdr_t，大小 128 字节
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DmaHeader {
    /// Magic number (高位)
    pub dmabuf_magic_m: u16,
    /// Magic number (低位)
    pub dmabuf_magic_s: u16,
    /// DMA buffer 总大小
    pub dmabuf_size: u32,
    /// CPU 描述符数量
    pub cpu_desc_count: u32,
    /// BD 描述符数量
    pub bd_desc_count: u32,
    /// TDMA 描述符数量
    pub tdma_desc_count: u32,
    /// TPU 时钟频率
    pub tpu_clk_rate: u32,
    /// PMU buffer 大小
    pub pmubuf_size: u32,
    /// PMU buffer 偏移
    pub pmubuf_offset: u32,
    /// Array base 0 低32位
    pub arraybase_0_l: u32,
    /// Array base 0 高32位
    pub arraybase_0_h: u32,
    /// Array base 1 低32位
    pub arraybase_1_l: u32,
    /// Array base 1 高32位
    pub arraybase_1_h: u32,
    /// Array base 2 低32位
    pub arraybase_2_l: u32,
    /// Array base 2 高32位
    pub arraybase_2_h: u32,
    /// Array base 3 低32位
    pub arraybase_3_l: u32,
    /// Array base 3 高32位
    pub arraybase_3_h: u32,
    /// Array base 4 低32位
    pub arraybase_4_l: u32,
    /// Array base 4 高32位
    pub arraybase_4_h: u32,
    /// Array base 5 低32位
    pub arraybase_5_l: u32,
    /// Array base 5 高32位
    pub arraybase_5_h: u32,
    /// Array base 6 低32位
    pub arraybase_6_l: u32,
    /// Array base 6 高32位
    pub arraybase_6_h: u32,
    /// Array base 7 低32位
    pub arraybase_7_l: u32,
    /// Array base 7 高32位
    pub arraybase_7_h: u32,
    /// 保留字段
    pub reserved: [u32; 8],
}

impl DmaHeader {
    /// 检查魔数是否有效
    pub fn is_valid(&self) -> bool {
        self.dmabuf_magic_m == super::TPU_DMABUF_HEADER_M
    }

    /// 检查 PMU buffer 是否有效且对齐
    pub fn has_valid_pmu(&self) -> bool {
        self.pmubuf_offset != 0
            && self.pmubuf_size != 0
            && (self.pmubuf_offset & 0xF) == 0
            && (self.pmubuf_size & 0xF) == 0
    }
}

/// CPU 同步描述符
/// 对应 C 结构体 cvi_cpu_sync_desc_t
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CpuSyncDesc {
    /// 操作类型
    pub op_type: u32,
    /// BD 命令数量
    pub num_bd: u32,
    /// GDMA 命令数量
    pub num_gdma: u32,
    /// BD 描述符偏移
    pub offset_bd: u32,
    /// GDMA 描述符偏移
    pub offset_gdma: u32,
    /// 保留字段
    pub reserved: [u32; 2],
    /// 字符串 (用于调试)
    pub str_data: [u8; (CPU_ENGINE_DESCRIPTOR_NUM - 7) * 4],
}

/// CPU 引擎描述符数量
pub const CPU_ENGINE_DESCRIPTOR_NUM: usize = 56;

/// 命令 ID 节点
#[derive(Debug, Clone, Copy, Default)]
pub struct CmdIdNode {
    /// BD 命令 ID
    pub bd_cmd_id: u32,
    /// TDMA 命令 ID
    pub tdma_cmd_id: u32,
}

/// TPU 平台配置
#[derive(Debug, Clone, Copy)]
pub struct TpuPlatformCfg {
    /// TDMA 基地址 (虚拟)
    pub tdma_base: *mut u8,
    /// TIU 基地址 (虚拟)
    pub tiu_base: *mut u8,
    /// PMU buffer 物理地址
    pub pmubuf_addr_p: u64,
    /// PMU buffer 大小
    pub pmubuf_size: u32,
}

/// PMU 事件类型
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpuPmuEvent {
    /// Bank 冲突
    BankConflict    = 0x0,
    /// Stall 计数
    StallCount      = 0x1,
    /// TDMA 带宽
    TdmaBandwidth   = 0x2,
    /// TDMA 写选通
    TdmaWriteStrobe = 0x3,
}

/// TDMA 寄存器描述符
/// 用于构建 TDMA 命令
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TdmaReg {
    pub vld: u32,
    pub compress_en: u32,
    pub eod: u32,
    pub intp_en: u32,
    pub bar_en: u32,
    pub check_bf16_value: u32,
    pub trans_dir: u32,
    pub rsv00: u32,
    pub trans_fmt: u32,
    pub transpose_md: u32,
    pub rsv01: u32,
    pub intra_cmd_paral: u32,
    pub outstanding_en: u32,
    pub cmd_id: u32,
    pub spec_func: u32,
    pub dst_fmt: u32,
    pub src_fmt: u32,
    pub cmprs_fmt: u32,
    pub sys_dtype: u32,
    pub rsv2_1: u32,
    pub int8_sign: u32,
    pub compress_zero_guard: u32,
    pub int8_rnd_mode: u32,
    pub wait_id_tpu: u32,
    pub wait_id_other_tdma: u32,
    pub wait_id_sdma: u32,
    pub const_val: u32,
    pub src_base_reg_sel: u32,
    pub mv_lut_idx: u32,
    pub dst_base_reg_sel: u32,
    pub mv_lut_base: u32,
    pub rsv4_5: u32,
    pub dst_h_stride: u32,
    pub dst_c_stride_low: u32,
    pub dst_n_stride: u32,
    pub src_h_stride: u32,
    pub src_c_stride_low: u32,
    pub src_n_stride: u32,
    pub dst_c: u32,
    pub src_c: u32,
    pub dst_w: u32,
    pub dst_h: u32,
    pub src_w: u32,
    pub src_h: u32,
    pub dst_base_addr_low: u32,
    pub src_base_addr_low: u32,
    pub src_n: u32,
    pub dst_base_addr_high: u32,
    pub src_base_addr_high: u32,
    pub src_c_stride_high: u32,
    pub dst_c_stride_high: u32,
    pub compress_bias0: u32,
    pub compress_bias1: u32,
    pub layer_id: u32,
}

impl TdmaReg {
    /// 创建新的 TDMA 寄存器描述符，使用默认值
    pub fn new() -> Self {
        Self {
            dst_fmt: 0x1,
            src_fmt: 0x1,
            dst_h_stride: 0x1,
            dst_c_stride_low: 0x1,
            dst_n_stride: 0x1,
            src_h_stride: 0x1,
            src_c_stride_low: 0x1,
            src_n_stride: 0x1,
            dst_c: 0x1,
            src_c: 0x1,
            dst_w: 0x1,
            dst_h: 0x1,
            src_w: 0x1,
            src_h: 0x1,
            src_n: 0x1,
            ..Default::default()
        }
    }

    /// 将寄存器描述符编码为 16 个 u32 数组
    pub fn emit(&self, out: &mut [u32; 16]) {
        out[15] = (self.compress_bias0 & 0xFF)
            | ((self.compress_bias1 & 0xFF) << 8)
            | ((self.layer_id & 0xFFFF) << 16);

        out[14] = (self.src_c_stride_high & 0xFFFF) | ((self.dst_c_stride_high & 0xFFFF) << 16);

        out[13] = (self.src_n & 0xFFFF)
            | ((self.dst_base_addr_high & 0xFF) << 16)
            | ((self.src_base_addr_high & 0xFF) << 24);

        out[12] = self.src_base_addr_low;
        out[11] = self.dst_base_addr_low;

        out[10] = (self.src_w & 0xFFFF) | ((self.src_h & 0xFFFF) << 16);
        out[9] = (self.dst_w & 0xFFFF) | ((self.dst_h & 0xFFFF) << 16);
        out[8] = (self.dst_c & 0xFFFF) | ((self.src_c & 0xFFFF) << 16);

        out[7] = self.src_n_stride;
        out[6] = (self.src_h_stride & 0xFFFF) | ((self.src_c_stride_low & 0xFFFF) << 16);
        out[5] = self.dst_n_stride;
        out[4] = (self.dst_h_stride & 0xFFFF) | ((self.dst_c_stride_low & 0xFFFF) << 16);

        out[3] = (self.const_val & 0xFFFF)
            | ((self.src_base_reg_sel & 0x7) << 16)
            | ((self.mv_lut_idx & 0x1) << 19)
            | ((self.dst_base_reg_sel & 0x7) << 20)
            | ((self.mv_lut_base & 0x1) << 23)
            | ((self.rsv4_5 & 0xFF) << 24);

        out[2] = (self.wait_id_other_tdma & 0xFFFF) | ((self.wait_id_sdma & 0xFFFF) << 16);

        out[1] = (self.spec_func & 0x7)
            | ((self.dst_fmt & 0x3) << 3)
            | ((self.src_fmt & 0x3) << 5)
            | ((self.cmprs_fmt & 0x1) << 7)
            | ((self.sys_dtype & 0x1) << 8)
            | ((self.rsv2_1 & 0xF) << 9)
            | ((self.int8_sign & 0x1) << 13)
            | ((self.compress_zero_guard & 0x1) << 14)
            | ((self.int8_rnd_mode & 0x1) << 15)
            | ((self.wait_id_tpu & 0xFFFF) << 16);

        out[0] = (self.vld & 0x1)
            | ((self.compress_en & 0x1) << 1)
            | ((self.eod & 0x1) << 2)
            | ((self.intp_en & 0x1) << 3)
            | ((self.bar_en & 0x1) << 4)
            | ((self.check_bf16_value & 0x1) << 5)
            | ((self.trans_dir & 0x3) << 6)
            | ((self.rsv00 & 0x3) << 8)
            | ((self.trans_fmt & 0x1) << 10)
            | ((self.transpose_md & 0x3) << 11)
            | ((self.rsv01 & 0x1) << 13)
            | ((self.intra_cmd_paral & 0x1) << 14)
            | ((self.outstanding_en & 0x1) << 15)
            | ((self.cmd_id & 0xFFFF) << 16);
    }
}
