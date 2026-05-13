//! RK3588 USB2 PHY 驱动
//!
//! 这个模块提供 USB2 PHY 的完整初始化功能，包括 RK3588 特定的 PHY 调优。
//! 参照 U-Boot 的 `drivers/phy/phy-rockchip-inno-usb2.c` 实现。

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;

use super::super::osal::Kernel;
use super::{
    CruOp,
    consts::genmask,
    udphy::{config::UdphyGrfReg, regmap::Regmap},
};
use crate::{Mmio, err::Result};

/// USB2PHY 寄存器偏移
pub mod reg_offset {

    /// HS DC 电压电平调整
    pub const HS_DC_LEVEL: u32 = 0x0004;
    /// 时钟控制和预加重配置
    pub const CLK_CONTROL: u32 = 0x0008;
    /// 挂起控制
    pub const SUSPEND_CONTROL: u32 = 0x000c;
}

/// USB2PHY GRF 寄存器配置（复用 UdphyGrfReg）
///
/// 对应 U-Boot 的 `struct usb2phy_reg`
pub type Usb2PhyGrfReg = UdphyGrfReg;

#[derive(Debug, Clone, Copy)]
#[repr(usize)]
pub enum Usb2PhyPortId {
    Otg,
    Host,
    Ports,
}

impl Usb2PhyPortId {
    pub fn from_node_name(name: &str) -> Option<Self> {
        match name {
            "otg-port" => Some(Usb2PhyPortId::Otg),
            "host-port" => Some(Usb2PhyPortId::Host),
            _ => None,
        }
    }
}

/// USB2PHY 端口配置
///
/// 对应 U-Boot 的 `struct rockchip_usb2phy_port_cfg`
#[derive(Clone)]
#[allow(dead_code)]
pub struct Usb2PhyPortCfg {
    /// PHY 挂起控制
    pub phy_sus: Usb2PhyGrfReg,
    /// UTMI 线路状态（OTG 模式使用）
    pub utmi_ls: Usb2PhyGrfReg,
    /// UTMI IDDIG 状态（OTG 模式，HOST 模式为 None）
    pub utmi_iddig: Usb2PhyGrfReg,
}

impl Usb2PhyPortCfg {
    pub const fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}

/// USB2PHY 配置
///
/// 对应 U-Boot 的 `struct rockchip_usb2phy_cfg`
#[derive(Clone)]
#[allow(dead_code)]
pub struct Usb2PhyCfg {
    pub reg: usize,
    /// 时钟输出控制（预留字段）
    pub clkout_ctl: Usb2PhyGrfReg,
    /// 端口配置
    pub port_cfg: [Usb2PhyPortCfg; Usb2PhyPortId::Ports as usize],
    /// PHY 调优函数指针（可选，针对特定 SoC）
    pub phy_tuning: fn(&Usb2Phy) -> Result<()>,
}

/// USB2PHY 初始化参数
///
/// 对应从设备树解析的信息
pub struct Usb2PhyParam<'a> {
    pub reg: usize,
    pub port_kind: Usb2PhyPortId,
    pub usb_grf: Mmio,
    /// 复位列表
    pub rst_list: &'a [(&'a str, u64)],
}

/// RK3588 USB2 PHY 驱动
pub struct Usb2Phy {
    grf: Regmap,
    port_kind: Usb2PhyPortId,
    /// 配置数据（共享引用）
    cfg: &'static Usb2PhyCfg,
    /// CRU 接口（用于复位控制）
    cru: Arc<dyn CruOp>,
    /// 复位信号映射表
    rsts: BTreeMap<String, u64>,
    kernel: Kernel,
}

impl Usb2Phy {
    /// 创建新的 USB2 PHY 实例（完整初始化）
    ///
    /// # Arguments
    ///
    /// * `base` - PHY 寄存器基址
    /// * `cru` - CRU 接口
    /// * `param` - 初始化参数
    pub fn new(cru: Arc<dyn CruOp>, param: Usb2PhyParam<'_>, kernel: Kernel) -> Self {
        // 根据 ID 选择对应的配置
        let cfg = find_usb2phy_cfg(param.reg);
        // 构建复位映射表
        let mut rsts = BTreeMap::new();
        for &(name, id) in param.rst_list.iter() {
            rsts.insert(String::from(name), id);
        }

        Usb2Phy {
            grf: Regmap::new(param.usb_grf),
            cfg,
            cru,
            rsts,
            port_kind: param.port_kind,
            kernel,
        }
    }

    fn write_reg(&self, offset: u32, value: u32) {
        self.grf.reg_write(offset, value);
    }

    fn read_reg(&self, offset: u32) -> u32 {
        self.grf.reg_read(offset)
    }

    /// 完整初始化 USB2 PHY
    ///
    /// 对应 U-Boot 的 `rockchip_usb2phy_init()`，执行完整的 PHY 初始化流程：
    /// 1. PHY 特定调优（RK3588 电压校准、预加重等）
    /// 2. 退出 PHY 挂起模式
    /// 3. 等待 UTMI 时钟稳定
    pub async fn setup(&mut self) -> Result<()> {
        info!("USB2PHY: Starting initialization");

        // Step 1: 执行 PHY 调优（如果配置了）
        (self.cfg.phy_tuning)(self)?;

        self.init();

        self.power_on();
        Ok(())
    }

    fn init(&self) {
        info!("USB2PHY: init with port kind {:?}", self.port_kind);
        let port_cfg = match self.port_kind {
            Usb2PhyPortId::Otg => &self.cfg.port_cfg[Usb2PhyPortId::Otg as usize],
            Usb2PhyPortId::Host => &self.cfg.port_cfg[Usb2PhyPortId::Host as usize],
            Usb2PhyPortId::Ports => {
                unreachable!()
            }
        };

        self.property_enable(&port_cfg.phy_sus, false);

        // Step 3: 等待 UTMI 时钟稳定（U-Boot 中等待 2ms）
        info!("USB2PHY: Waiting for UTMI clock to stabilize",);
        self.kernel.delay(core::time::Duration::from_micros(2000));
    }

    fn property_enable(&self, reg: &Usb2PhyGrfReg, en: bool) {
        let tmp = if en { reg.enable } else { reg.disable };
        let mask = genmask(reg.bitend, reg.bitstart) as u32;
        let val = (tmp << reg.bitstart) | (mask << 16);
        self.write_reg(reg.offset, val);
    }

    /// 执行 PHY 复位
    ///
    /// 复位时序：assert 20μs → deassert 100μs
    fn reset(&self) {
        // Assert reset
        if let Some(&rst_id) = self.rsts.get("phy") {
            self.cru.reset_assert(rst_id);
            self.kernel.delay(core::time::Duration::from_micros(20));

            // Deassert reset
            self.cru.reset_deassert(rst_id);
            self.kernel.delay(core::time::Duration::from_micros(100));
        }
    }

    fn power_on(&self) {}

    /// 打印 USB2 PHY 关键寄存器状态（用于调试）
    pub fn dump_registers(&self) {
        info!("=== USB2 PHY Register Dump ===");
        info!("PHY Base: 0x{:08x}", self.grf.base());

        // 读取并解析 CLK_CONTROL 寄存器
        let clk_ctrl = self.read_reg(reg_offset::CLK_CONTROL);
        info!("CLK_CONTROL (0x0008) = 0x{:08x}", clk_ctrl);
        info!(
            "  IDDQ (bit 29)    = {} ({})",
            (clk_ctrl >> 29) & 0x1,
            if (clk_ctrl >> 29) & 0x1 == 0 {
                "正常工作模式 ✅"
            } else {
                "IDDQ 低功耗模式 ❌"
            }
        );
        info!(
            "  HS_TX_PREEMP (bits 20:19) = {} ({})",
            (clk_ctrl >> 19) & 0x3,
            match (clk_ctrl >> 19) & 0x3 {
                0b00 => "0x",
                0b01 => "1x",
                0b10 => "2x (推荐) ✅",
                0b11 => "3x",
                _ => "未知",
            }
        );

        // 读取并解析 HS_DC_LEVEL 寄存器
        let hs_dc = self.read_reg(reg_offset::HS_DC_LEVEL);
        info!("HS_DC_LEVEL (0x0004) = 0x{:08x}", hs_dc);
        info!(
            "  HS_DC_LEVEL (bits 27:24) = 0x{:x} ({})",
            (hs_dc >> 24) & 0xf,
            match (hs_dc >> 24) & 0xf {
                0x9 => "+5.89% (推荐) ✅",
                _ => "其他值",
            }
        );

        // 读取并解析 SUSPEND_CONTROL 寄存器
        let suspend = self.read_reg(reg_offset::SUSPEND_CONTROL);
        info!("SUSPEND_CONTROL (0x000c) = 0x{:08x}", suspend);
        info!("  PHY_SUSPEND (bit 11) = {}", (suspend >> 11) & 0x1);
        info!("=========================");
    }
}

/// RK3588 USB2PHY 调优函数
///
/// 对应 U-Boot 的 `rk3588_usb2phy_tuning()`，执行 RK3588 特定的 PHY 调优：
/// 1. 退出 IDDQ 模式（低功耗）
/// 2. 执行复位序列
/// 3. HS DC 电压校准（+5.89%）
/// 4. 预加重设置（2x）
fn rk3588_usb2phy_tuning(phy: &Usb2Phy) -> Result<()> {
    info!("USB2PHY: Applying RK3588-specific tuning");

    // Step 1: 退出 IDDQ 模式
    // U-Boot: regmap_write(base, 0x0008, GENMASK(29, 29) | 0x0000)
    // Bit[29:29] = IDDQ
    phy.write_reg(
        reg_offset::CLK_CONTROL,
        genmask(29, 29) as u32, // mask=bit29, value=0
    );

    // Step 2: 执行复位
    phy.reset();

    // Step 3: HS DC 电压校准
    // U-Boot: regmap_write(base, 0x0004, GENMASK(27, 24) | 0x0900)
    // Bit[27:24] = HS_DC_LEVEL, 设置为 0b1001 (+5.89%)
    phy.write_reg(
        reg_offset::HS_DC_LEVEL,
        genmask(27, 24) as u32 | 0x0900, // mask=bits[27:24], value=0x0900
    );

    // Step 4: 预加重设置
    // U-Boot: regmap_write(base, 0x0008, GENMASK(20, 19) | 0x0010)
    // Bit[20:19] = HS_TX_PREEMP, 设置为 0b10 (2x)
    phy.write_reg(
        reg_offset::CLK_CONTROL,
        genmask(20, 19) as u32 | 0x0010, // mask=bits[20:19], value=0x0010
    );
    info!("USB2PHY: HS transmitter pre-emphasis set to 2x",);

    // 打印寄存器状态以便验证
    phy.dump_registers();

    Ok(())
}

fn find_usb2phy_cfg(reg: usize) -> &'static Usb2PhyCfg {
    for c in RK3588_PHY_CFGS.iter() {
        if c.reg == reg {
            return c;
        } else {
            continue;
        }
    }
    unreachable!("unsupported USB2PHY reg: {:#x}", reg)
}

const RK3588_PHY_CFGS: &[Usb2PhyCfg] = &[
    Usb2PhyCfg {
        reg: 0x0,
        clkout_ctl: Usb2PhyGrfReg::new(0x0000, 0, 0, 1, 0),
        port_cfg: [
            // OTG 端口配置
            Usb2PhyPortCfg {
                phy_sus: Usb2PhyGrfReg::new(0x000c, 11, 11, 0, 1),
                utmi_ls: Usb2PhyGrfReg::new(0x00c0, 10, 9, 0, 1),
                utmi_iddig: Usb2PhyGrfReg::new(0x00c0, 5, 5, 0, 1),
            },
            Usb2PhyPortCfg::default(),
        ],
        phy_tuning: rk3588_usb2phy_tuning,
    },
    Usb2PhyCfg {
        reg: 0x4000,
        clkout_ctl: Usb2PhyGrfReg::new(0x0000, 0, 0, 1, 0),
        port_cfg: [
            // OTG 端口配置
            Usb2PhyPortCfg {
                phy_sus: Usb2PhyGrfReg::new(0x000c, 11, 11, 0, 0),
                utmi_ls: Usb2PhyGrfReg::new(0x00c0, 10, 9, 0, 1),
                utmi_iddig: Usb2PhyGrfReg::default(),
            },
            Usb2PhyPortCfg::default(),
        ],
        phy_tuning: rk3588_usb2phy_tuning,
    },
];
