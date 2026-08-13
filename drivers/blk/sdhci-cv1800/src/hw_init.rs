//! SG2002 SDIO1 硬件初始化 (时钟 / 复位 / Pinmux / CardDetect)

use crate::{mmio_read, mmio_write};
/// clk_en_0 (offset 0x000)
///   bit21: clk_axi4_sd1  bit22: clk_sd1  bit23: clk_100k_sd1
const CLK_EN_0: usize = 0x000;
const CLK_EN_0_SD1_ALL: u32 = (1 << 21) | (1 << 22) | (1 << 23);

/// clk_byp_0 (offset 0x030)
///   bit7: clk_sd1 bypass — 1=xtal, 0=PLL
const CLK_BYP_0: usize = 0x030;
const CLK_BYP_0_SD1: u32 = 1 << 7;

/// div_clk_sd1 (offset 0x07C)
///   bit0: Divider Reset Control — 0=assert, 1=de-assert
const DIV_CLK_SD1: usize = 0x07C;

/// div_clk_100k_sd1 (offset 0x084)
///   bit0: Divider Reset Control — 0=assert, 1=de-assert
const DIV_CLK_100K_SD1: usize = 0x084;

/// Divider reset de-assert (bit 0 = 1)
const DIV_RESET_DEASSERT: u32 = 1 << 0;

/// sd_ctrl_opt (offset 0x294)
///   bit8: reg_sd1_carddet_ow — 使能卡检测覆写
///   bit9: reg_sd1_carddet_sw — 覆写值 (1=卡已插入)
const SD_CTRL_OPT: usize = 0x294;
const SD1_CARDDET_OW: u32 = 1 << 8;
const SD1_CARDDET_SW: u32 = 1 << 9;

/// rtcsys_rst_ctrl (offset 0x018)
///   bit2: reg_soft_rstn_sdio — 0=reset, 1=de-assert
const RTCSYS_RST_CTRL: usize = 0x018;
const RTCSYS_RST_SDIO: u32 = 1 << 2;

/// rtcsys_clkmux (offset 0x01C)
///   bits[3:0]: reg_sdio_clk_mux — 0=fpll/4, 1=osc_div
const RTCSYS_CLKMUX: usize = 0x01C;
/// bits[3:0] 掩码，用于清除时钟源选择位
const RTCSYS_CLKMUX_MASK: u32 = 0xF;

/// rtcsys_clkbyp (offset 0x030)
///   bit1: clk_sdio — 0=PLL, 1=xtal
const RTCSYS_CLKBYP: usize = 0x030;
const RTCSYS_CLKBYP_SDIO: u32 = 1 << 1;

/// rtcsys_clk_en (offset 0x034)
///   bit1: clk_sd1  bit2: clk_fab_sd1
const RTCSYS_CLK_EN: usize = 0x034;
const RTCSYS_CLK_EN_SD1_ALL: u32 = (1 << 1) | (1 << 2);

/// SD1/VO 引脚功能选择 (offset 0x0E4)
///   写 0 = SD1 功能, 非零 = VO 功能
const FMUX_SD1_VO: usize = 0x0E4;
/// 选择 SD1 引脚功能
const FMUX_SEL_SD1: u32 = 0x0;

#[inline]
fn mmio_set_bits32(addr: usize, bits: u32) {
    mmio_write::<u32>(addr, mmio_read::<u32>(addr) | bits);
}

#[inline]
fn mmio_clr_bits32(addr: usize, bits: u32) {
    mmio_write::<u32>(addr, mmio_read::<u32>(addr) & !bits);
}

/// SDIO1 硬件初始化所需的 SoC 子系统基址
pub struct Sdio1HwConfig {
    /// CRG (Clock/Reset Generator) 虚拟地址
    pub crg_base_va: usize,
    /// System Control (TOP_MISC) 虚拟地址
    pub sysctrl_base_va: usize,
    /// RTC 子系统控制寄存器虚拟地址
    pub rtcsys_ctrl_base_va: usize,
    /// RTC 子系统 IO 复用寄存器虚拟地址
    pub rtcsys_io_base_va: usize,
    /// SDIO1 控制器虚拟地址
    pub sdio1_base_va: usize,
}

impl Sdio1HwConfig {
    /// 从平台物理地址和 phys-virt offset 构造虚拟地址
    pub fn new(
        crg_paddr: usize,
        sysctrl_paddr: usize,
        rtcsys_ctrl_paddr: usize,
        rtcsys_io_paddr: usize,
        sdio1_paddr: usize,
        phys_virt_offset: usize,
    ) -> Self {
        Self {
            crg_base_va: crg_paddr + phys_virt_offset,
            sysctrl_base_va: sysctrl_paddr + phys_virt_offset,
            rtcsys_ctrl_base_va: rtcsys_ctrl_paddr + phys_virt_offset,
            rtcsys_io_base_va: rtcsys_io_paddr + phys_virt_offset,
            sdio1_base_va: sdio1_paddr + phys_virt_offset,
        }
    }
}

/// SDIO1 SoC 级硬件使能: Pinmux → 时钟 → 复位 → 卡检测覆写
pub fn sdio1_hw_init(cfg: &Sdio1HwConfig) {
    // 0. FMUX per-pin FSEL: 将 SD1_D3/D2/D1/D0/CMD/CLK 从 SPI NOR 切换到 SD1
    //    FMUX 基址 = SYSCON + 0x1000, SD1 引脚 FSEL 在偏移 0xD0-0xE4
    //    默认 FSEL=6 (SPI NOR1), 写 0 选择 SD1 功能
    let fmux_base = cfg.sysctrl_base_va + 0x1000;
    for offset in [0xD0u32, 0xD4, 0xD8, 0xDC, 0xE0, 0xE4] {
        let addr = fmux_base + offset as usize;
        let prev = mmio_read::<u32>(addr);
        if prev & 0x7 != 0 {
            mmio_write::<u32>(addr, prev & !0x7); // FSEL = 0 = SD1
        }
    }

    // 0b. IOBLK GRTC pad control: 为 RTC 域 SD1 引脚启用 pull-up
    //     Linux pinctrl cvitek_pinctrl_unlock() 对 0x0502_7088..0x0502_70D8
    //     的 20 个寄存器写 0x11111111, 每字节 bit[4]=1 表示 pull-up 使能
    //     SD1_DAT0-DAT3/CMD/CLK 均为 RTC 域引脚, 需要 pull-up 保证信号完整
    for i in 0..20u32 {
        let addr = cfg.rtcsys_io_base_va + 0x88 + (i as usize) * 4;
        mmio_write::<u32>(addr, 0x11111111);
    }

    // 1. Pinmux: 选择 SD1 功能 (与 VO[32..37] 复用) — bulk override
    mmio_write::<u32>(cfg.rtcsys_io_base_va + FMUX_SD1_VO, FMUX_SEL_SD1);

    // 2. CRG 主系统时钟
    //    2a. 使能 clk_axi4_sd1 / clk_sd1 / clk_100k_sd1
    mmio_set_bits32(cfg.crg_base_va + CLK_EN_0, CLK_EN_0_SD1_ALL);
    //    2b. 关闭 bypass, 使用 PLL 时钟源
    mmio_clr_bits32(cfg.crg_base_va + CLK_BYP_0, CLK_BYP_0_SD1);
    //    2c. 解除分频器复位
    mmio_set_bits32(cfg.crg_base_va + DIV_CLK_SD1, DIV_RESET_DEASSERT);
    mmio_set_bits32(cfg.crg_base_va + DIV_CLK_100K_SD1, DIV_RESET_DEASSERT);

    // 3. RTC 域时钟
    //    3a. 时钟源选择 fpll/4 (清除 bits[3:0])
    let addr = cfg.rtcsys_ctrl_base_va + RTCSYS_CLKMUX;
    mmio_write::<u32>(addr, mmio_read::<u32>(addr) & !RTCSYS_CLKMUX_MASK);
    //    3b. 使能 clk_sd1 / clk_fab_sd1
    mmio_set_bits32(
        cfg.rtcsys_ctrl_base_va + RTCSYS_CLK_EN,
        RTCSYS_CLK_EN_SD1_ALL,
    );
    //    3c. 关闭 bypass, 使用 PLL
    mmio_clr_bits32(cfg.rtcsys_ctrl_base_va + RTCSYS_CLKBYP, RTCSYS_CLKBYP_SDIO);

    // 4. 解除 RTC 域 SDIO 复位
    //    注: 不操作 SOFT_RSTN_0 bit17 (TRM 标记为 Reserved)
    mmio_set_bits32(cfg.rtcsys_ctrl_base_va + RTCSYS_RST_CTRL, RTCSYS_RST_SDIO);

    // 5. SoC 级卡检测覆写 (WiFi 模块无 CD 引脚)
    mmio_set_bits32(
        cfg.sysctrl_base_va + SD_CTRL_OPT,
        SD1_CARDDET_OW | SD1_CARDDET_SW,
    );

    // 6. 等待时钟和复位稳定 (Linux: CCF 内建 PLL lock 等待)
    crate::delay_ms(1);
}
