use core::time::Duration;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;

use super::{
    CruOp,
    udphy::regmap::{RK3588_UDPHY_24M_REFCLK_CFG, RK3588_UDPHY_INIT_SEQUENCE, Regmap},
};
use crate::{
    Mmio,
    err::Result,
    osal::{Kernel, SpinWhile},
};

pub mod config;
mod consts;
pub mod regmap;

use consts::*;
use tock_registers::{interfaces::*, registers::*};

// RK3588 VO GRF 寄存器定义
const RK3588_GRF_VO0_CON0: u32 = 0x0000;
const RK3588_GRF_VO0_CON2: u32 = 0x0008;

// DP 位定义
const DP_AUX_DIN_SEL: u32 = 1 << 9;
const DP_AUX_DOUT_SEL: u32 = 1 << 8;
const DP_LANE_SEL_ALL: u32 = 0xFF;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UdphyMode: u8 {
        const NONE = 0;
        const USB = 1;
        const DP = 1 << 1;
        const DP_USB = Self::DP.bits() | Self::USB.bits();
    }
}

/// USBDP PHY 寄存器偏移
pub const UDPHY_PMA: usize = 0x8000;

pub struct UdphyParam<'a> {
    pub id: usize,
    /// prop `rockchip,usb2phy-grf`
    pub u2phy_grf: Mmio,
    /// prop `rockchip,usb-grf`
    pub usb_grf: Mmio,
    /// prop `rockchip,usbdpphy-grf`
    pub usbdpphy_grf: Mmio,
    /// prop `rockchip,vo-grf`
    pub vo_grf: Mmio,
    /// prop `rockchip,dp-lane-mux`
    pub dp_lane_mux: &'a [u32],
    pub rst_list: &'a [(&'a str, u64)],
}

pub struct Udphy {
    id: usize,
    cfg: Box<config::UdphyCfg>,
    mode: UdphyMode,
    /// PHY MMIO 基址
    phy_base: usize,

    pma_remap: Regmap,
    /// USBDP PHY GRF
    udphygrf: Regmap,
    /// USB GRF
    usb_grf: Regmap,
    /// VO GRF (用于 DP lane 选择)
    vo_grf: Regmap,
    // /// USB2PHY GRF
    // usb2phy_grf: Grf,
    lane_mux_sel: [u32; 4],
    dp_lane_sel: [u32; 4],
    /// Type C 反转标志
    flip: bool,
    cru: Arc<dyn CruOp>,
    rsts: BTreeMap<String, u64>,
}

impl Udphy {
    pub fn new(base: Mmio, cru: Arc<dyn CruOp>, param: UdphyParam<'_>) -> Self {
        let cfg = Box::new(config::RK3588_UDPHY_CFGS.clone());
        let mut lane_mux_sel = [0u32; 4];
        let mut dp_lane_sel = [0u32; 4];

        // 完全按照 U-Boot 的逻辑：udphy_parse_lane_mux_data()
        let mut mode;
        let mut flip = false;

        if param.dp_lane_mux.is_empty() {
            // 没有找到 dp-lane-mux 属性 → 纯 USB 模式
            mode = UdphyMode::USB;
            info!("Udphy: No dp-lane-mux property, using USB-only mode");
        } else {
            // 有 dp-lane-mux 属性
            let num_lanes = param.dp_lane_mux.len();

            if num_lanes != 2 && num_lanes != 4 {
                panic!("Invalid number of lane mux: {}", num_lanes);
            }

            // 解析 lane mux 配置
            for (i, &lane) in param.dp_lane_mux.iter().enumerate() {
                if lane > 3 {
                    panic!("Lane mux must be between 0 and 3, got {}", lane);
                }
                lane_mux_sel[lane as usize] = PHY_LANE_MUX_DP;
                dp_lane_sel[i] = lane;
            }

            mode = UdphyMode::DP; // 默认是 DP 模式
            if num_lanes == 2 {
                // 2 个 lanes → USB + DP 混合模式
                mode |= UdphyMode::USB;
                flip = lane_mux_sel[0] == PHY_LANE_MUX_DP;
                info!("Udphy: Configured for USB+DP mode with 2 DP lanes");
            } else {
                // 4 个 lanes → 纯 DP 模式
                info!("Udphy: Configured for DP-only mode with 4 DP lanes");
            }

            // 输出 lane 配置信息（参考 U-Boot 的 debug 输出）
            debug!("dp_lane_sel: {:?}", dp_lane_sel);
            debug!("lane_mux_sel: {:?}", lane_mux_sel);
        }

        let mut rsts = BTreeMap::new();
        for &(name, id) in param.rst_list.iter() {
            if cfg.rst_list.contains(&name) {
                rsts.insert(String::from(name), id);
            } else {
                panic!("unsupported reset name: {}", name);
            }
        }

        Udphy {
            id: param.id,
            cfg,
            mode,
            phy_base: base.as_ptr() as usize,
            pma_remap: Regmap::new(unsafe { base.add(UDPHY_PMA) }),
            udphygrf: Regmap::new(param.usbdpphy_grf),
            usb_grf: Regmap::new(param.usb_grf),
            vo_grf: Regmap::new(param.vo_grf),
            lane_mux_sel,
            dp_lane_sel,
            cru,
            rsts,
            flip,
        }
    }

    pub async fn setup(&mut self, kernel: &Kernel) -> Result<()> {
        info!("Starting initialization");
        for &rst in self.cfg.rst_list {
            self.reset_assert(rst);
        }

        // enable rx lfps for usb
        if self.mode.contains(UdphyMode::USB) {
            debug!("Enabling RX LFPS for USB mode");
            self.udphygrf.grfreg_write(&self.cfg.grf.rx_lfps, true);
        }

        // Step 1: power on pma and deassert apb rstn
        self.udphygrf.grfreg_write(&self.cfg.grf.low_pwrn, true);

        self.reset_deassert("pma_apb");
        self.reset_deassert("pcs_apb");
        debug!("PMA powered on and APB resets deasserted");

        // Step 2: set init sequence and phy refclk
        self.pma_remap.multi_reg_write(RK3588_UDPHY_INIT_SEQUENCE);

        debug!("Initial register sequences applied");

        self.pma_remap.multi_reg_write(RK3588_UDPHY_24M_REFCLK_CFG);

        debug!("24M reference clock configured");

        // Step 3: configure lane mux
        self.cmn_lane_mux_and_en().write(
            CMN_LANE_MUX_EN::LANE0_MUX.val(self.lane_mux_sel[0])
                + CMN_LANE_MUX_EN::LANE1_MUX.val(self.lane_mux_sel[1])
                + CMN_LANE_MUX_EN::LANE2_MUX.val(self.lane_mux_sel[2])
                + CMN_LANE_MUX_EN::LANE3_MUX.val(self.lane_mux_sel[3])
                + CMN_LANE_MUX_EN::LANE0_EN::Disable
                + CMN_LANE_MUX_EN::LANE1_EN::Disable
                + CMN_LANE_MUX_EN::LANE2_EN::Disable
                + CMN_LANE_MUX_EN::LANE3_EN::Disable,
        );
        // Step 4: deassert init rstn and wait for 200ns from datasheet
        if self.mode.contains(UdphyMode::USB) {
            self.reset_deassert("init");
        }

        if self.mode.contains(UdphyMode::DP) {
            self.cmn_dp_rstn().modify(CMN_DP_RSTN::DP_INIT_RSTN::Enable);
        }

        kernel.delay(Duration::from_micros(1));

        if self.mode.contains(UdphyMode::USB) {
            // Step 5: deassert usb rstn
            self.reset_deassert("cmn");
            self.reset_deassert("lane");
        }
        //  Step 6: wait for lock done of pll
        self.status_check().await;
        info!("Udphy initialized");

        self.u3_port_disable(!self.mode.contains(UdphyMode::USB));

        let dplanes = self.dplane_get();
        debug!(
            "Configured for {:?} mode with {} DP lanes",
            self.mode, dplanes
        );
        self.dplane_enable(dplanes);
        self.dplane_select();

        // 打印寄存器状态以便验证
        self.dump_registers();

        Ok(())
    }

    /// 选择 DP lane（配置 VO GRF 寄存器）
    ///
    /// 完全按照 U-Boot 的逻辑：rk3588_udphy_dplane_select()
    fn dplane_select(&self) {
        let mut value = 0u32;

        match self.mode {
            UdphyMode::DP => {
                // 4 lanes: 配置所有 4 个 lanes
                value |= 0u32 << (self.dp_lane_sel[0] * 2);
                value |= 1u32 << (self.dp_lane_sel[1] * 2);
                value |= 2u32 << (self.dp_lane_sel[2] * 2);
                value |= 3u32 << (self.dp_lane_sel[3] * 2);
            }
            UdphyMode::DP_USB => {
                // 2 lanes: 只配置 lane 0 和 lane 1
                value |= 0u32 << (self.dp_lane_sel[0] * 2);
                value |= 1u32 << (self.dp_lane_sel[1] * 2);
            }
            UdphyMode::USB => {
                // 纯 USB 模式：不配置 DP lane
                debug!("Udphy: USB-only mode, skipping DP lane selection");
                return;
            }
            _ => {
                debug!("Udphy: Unknown mode, skipping DP lane selection");
                return;
            }
        }

        // 选择 VO GRF 寄存器（id 0 用 CON0，id 1 用 CON2）
        let reg_offset = if self.id > 0 {
            RK3588_GRF_VO0_CON2
        } else {
            RK3588_GRF_VO0_CON0
        };

        // 构造写入值：
        // mask = DP_AUX_DIN_SEL | DP_AUX_DOUT_SEL | DP_LANE_SEL_ALL
        // 默认 dp_aux_din_sel = 0, dp_aux_dout_sel = 0
        let mask = (DP_AUX_DIN_SEL | DP_AUX_DOUT_SEL | DP_LANE_SEL_ALL) << 16;
        let dp_aux_val = 0; // dp_aux_din_sel 和 dp_aux_dout_sel 都设为 0

        let final_value = mask | dp_aux_val | value;

        debug!(
            "Udphy: Writing VO GRF register 0x{:03x} with value 0x{:08x} (lane value: 0x{:02x})",
            reg_offset, final_value, value
        );

        self.vo_grf.reg_write(reg_offset, final_value);
    }

    fn dplane_enable(&self, lanes: usize) {
        // Disable all DP lanes and assert common reset when DP is unused
        if lanes == 0 {
            self.cmn_lane_mux_and_en().modify(
                CMN_LANE_MUX_EN::LANE0_EN::Disable
                    + CMN_LANE_MUX_EN::LANE1_EN::Disable
                    + CMN_LANE_MUX_EN::LANE2_EN::Disable
                    + CMN_LANE_MUX_EN::LANE3_EN::Disable,
            );
            self.cmn_dp_rstn().modify(CMN_DP_RSTN::DP_CMN_RSTN::Reset);
            return;
        }

        // Enable only the lanes actually muxed to DP according to dp_lane_mux
        let mut fv = CMN_LANE_MUX_EN::LANE0_EN::Disable
            + CMN_LANE_MUX_EN::LANE1_EN::Disable
            + CMN_LANE_MUX_EN::LANE2_EN::Disable
            + CMN_LANE_MUX_EN::LANE3_EN::Disable;

        for (idx, sel) in self.lane_mux_sel.iter().enumerate() {
            if *sel == PHY_LANE_MUX_DP {
                fv += match idx {
                    0 => CMN_LANE_MUX_EN::LANE0_EN::Enable,
                    1 => CMN_LANE_MUX_EN::LANE1_EN::Enable,
                    2 => CMN_LANE_MUX_EN::LANE2_EN::Enable,
                    3 => CMN_LANE_MUX_EN::LANE3_EN::Enable,
                    _ => unreachable!(),
                };
            }
        }
        // let fv = CMN_LANE_MUX_EN::LANE0_EN::Enable
        //     + CMN_LANE_MUX_EN::LANE1_EN::Enable
        //     + CMN_LANE_MUX_EN::LANE2_EN::Enable
        //     + CMN_LANE_MUX_EN::LANE3_EN::Enable;

        self.cmn_lane_mux_and_en().modify(fv);
    }

    fn dplane_get(&self) -> usize {
        match self.mode {
            UdphyMode::DP => 4,
            UdphyMode::DP_USB => 2,
            _ => 0,
        }
    }

    async fn status_check(&self) {
        if self.mode.contains(UdphyMode::USB) {
            debug!("Waiting for PLL lock...");
            SpinWhile::new(|| {
                !self.cmn_ana_lcpll().is_set(CMN_ANA_LCPLL::AFC_DONE)
                    || !self.cmn_ana_lcpll().is_set(CMN_ANA_LCPLL::LOCK_DONE)
            })
            .await;

            if self.flip {
                SpinWhile::new(|| {
                    !self
                        .trsv_ln2_mon_rx_cdr()
                        .is_set(TRSV_LN2_MON_RX_CDR::LOCK_DONE)
                })
                .await;
            } else {
                SpinWhile::new(|| {
                    !self
                        .trsv_ln0_mon_rx_cdr()
                        .is_set(TRSV_LN0_MON_RX_CDR::LOCK_DONE)
                })
                .await;
            }
        }
    }

    pub fn u3_port_disable(&self, disable: bool) {
        debug!("udphy{}: u3 port set disable: {disable}", self.id);

        let cfg = if self.id > 0 {
            &self.cfg.grf.usb3otg1_cfg
        } else {
            &self.cfg.grf.usb3otg0_cfg
        };

        self.usb_grf.grfreg_write(cfg, disable);
    }

    fn cmn_lane_mux_and_en(&self) -> &ReadWrite<u32, CMN_LANE_MUX_EN::Register> {
        unsafe { &*((self.phy_base + UDPHY_PMA + pma_offset::CMN_LANE_MUX_AND_EN) as *const _) }
    }

    fn cmn_dp_rstn(&self) -> &ReadWrite<u32, CMN_DP_RSTN::Register> {
        unsafe { &*((self.phy_base + UDPHY_PMA + pma_offset::CMN_DP_RSTN) as *const _) }
    }

    fn cmn_ana_lcpll(&self) -> &ReadWrite<u32, CMN_ANA_LCPLL::Register> {
        unsafe { &*((self.phy_base + UDPHY_PMA + pma_offset::CMN_ANA_LCPLL_DONE) as *const _) }
    }

    fn trsv_ln0_mon_rx_cdr(&self) -> &ReadOnly<u32, TRSV_LN0_MON_RX_CDR::Register> {
        unsafe { &*((self.phy_base + UDPHY_PMA + pma_offset::TRSV_LN0_MON_RX_CDR) as *const _) }
    }

    fn trsv_ln2_mon_rx_cdr(&self) -> &ReadOnly<u32, TRSV_LN2_MON_RX_CDR::Register> {
        unsafe { &*((self.phy_base + UDPHY_PMA + pma_offset::TRSV_LN2_MON_RX_CDR) as *const _) }
    }

    fn reset_assert(&self, name: &str) {
        if let Some(&rst_id) = self.rsts.get(name) {
            self.cru.reset_assert(rst_id);
        } else {
            panic!("unsupported reset name: {}", name);
        }
    }

    fn reset_deassert(&self, name: &str) {
        if let Some(&rst_id) = self.rsts.get(name) {
            self.cru.reset_deassert(rst_id);
        } else {
            panic!("unsupported reset name: {}", name);
        }
    }

    /// 打印 USB3/DP PHY 关键寄存器状态（用于调试）
    fn dump_registers(&self) {
        info!("=== USB3/DP PHY Register Dump ===");
        info!("PHY ID: {}", self.id);
        info!("PHY Mode: {:?}", self.mode);
        info!("PHY Base: 0x{:08x}", self.phy_base);

        // 打印 Lane MUX 配置
        let lane_mux = self.cmn_lane_mux_and_en().extract();
        info!("CMN_LANE_MUX_AND_EN = 0x{:08x}", lane_mux.get());
        info!(
            "  LANE0_MUX = {} ({})",
            lane_mux.read(CMN_LANE_MUX_EN::LANE0_MUX),
            self.lane_mux_name(lane_mux.read(CMN_LANE_MUX_EN::LANE0_MUX))
        );
        info!(
            "  LANE1_MUX = {} ({})",
            lane_mux.read(CMN_LANE_MUX_EN::LANE1_MUX),
            self.lane_mux_name(lane_mux.read(CMN_LANE_MUX_EN::LANE1_MUX))
        );
        info!(
            "  LANE2_MUX = {} ({})",
            lane_mux.read(CMN_LANE_MUX_EN::LANE2_MUX),
            self.lane_mux_name(lane_mux.read(CMN_LANE_MUX_EN::LANE2_MUX))
        );
        info!(
            "  LANE3_MUX = {} ({})",
            lane_mux.read(CMN_LANE_MUX_EN::LANE3_MUX),
            self.lane_mux_name(lane_mux.read(CMN_LANE_MUX_EN::LANE3_MUX))
        );
        info!(
            "  LANE0_EN = {}",
            if lane_mux.read(CMN_LANE_MUX_EN::LANE0_EN) == 0 {
                "Disabled"
            } else {
                "Enabled ✅"
            }
        );
        info!(
            "  LANE1_EN = {}",
            if lane_mux.read(CMN_LANE_MUX_EN::LANE1_EN) == 0 {
                "Disabled"
            } else {
                "Enabled ✅"
            }
        );
        info!(
            "  LANE2_EN = {}",
            if lane_mux.read(CMN_LANE_MUX_EN::LANE2_EN) == 0 {
                "Disabled"
            } else {
                "Enabled ✅"
            }
        );
        info!(
            "  LANE3_EN = {}",
            if lane_mux.read(CMN_LANE_MUX_EN::LANE3_EN) == 0 {
                "Disabled"
            } else {
                "Enabled ✅"
            }
        );

        // 打印 PLL 锁定状态
        let lcpll = self.cmn_ana_lcpll().extract();
        info!("CMN_ANA_LCPLL_DONE = 0x{:08x}", lcpll.get());
        info!(
            "  AFC_DONE = {}",
            if lcpll.is_set(CMN_ANA_LCPLL::AFC_DONE) {
                "Locked ✅"
            } else {
                "Not Locked ❌"
            }
        );
        info!(
            "  LOCK_DONE = {}",
            if lcpll.is_set(CMN_ANA_LCPLL::LOCK_DONE) {
                "Locked ✅"
            } else {
                "Not Locked ❌"
            }
        );

        // 打印 CDR 锁定状态（根据 flip 选择 lane 0 或 lane 2）
        if self.mode.contains(UdphyMode::USB) {
            if self.flip {
                let cdr = self.trsv_ln2_mon_rx_cdr().extract();
                info!("TRSV_LN2_MON_RX_CDR = 0x{:08x}", cdr.get());
                info!(
                    "  LOCK_DONE (Lane 2) = {}",
                    if cdr.is_set(TRSV_LN2_MON_RX_CDR::LOCK_DONE) {
                        "Locked ✅"
                    } else {
                        "Not Locked ❌"
                    }
                );
            } else {
                let cdr = self.trsv_ln0_mon_rx_cdr().extract();
                info!("TRSV_LN0_MON_RX_CDR = 0x{:08x}", cdr.get());
                info!(
                    "  LOCK_DONE (Lane 0) = {}",
                    if cdr.is_set(TRSV_LN0_MON_RX_CDR::LOCK_DONE) {
                        "Locked ✅"
                    } else {
                        "Not Locked ❌"
                    }
                );
            }
        }

        // 打印 DP Reset 状态
        let dp_rstn = self.cmn_dp_rstn().extract();
        info!("CMN_DP_RSTN = 0x{:08x}", dp_rstn.get());
        info!(
            "  DP_CMN_RSTN = {}",
            if dp_rstn.read(CMN_DP_RSTN::DP_CMN_RSTN) == 1 {
                "Released ✅"
            } else {
                "Asserted"
            }
        );
        if self.mode.contains(UdphyMode::DP) {
            info!(
                "  DP_INIT_RSTN = {}",
                if dp_rstn.read(CMN_DP_RSTN::DP_INIT_RSTN) == 1 {
                    "Released ✅"
                } else {
                    "Asserted"
                }
            );
        }

        info!(
            "  U3 Port Disable = {}",
            !self.mode.contains(UdphyMode::USB)
        );
        info!("================================");
    }

    fn lane_mux_name(&self, val: u32) -> &'static str {
        match val {
            0 => "USB",
            1 => "DP",
            2 => "Reserved",
            3 => "Reserved",
            _ => "Unknown",
        }
    }
}
