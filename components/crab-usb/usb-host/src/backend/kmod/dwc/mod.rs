//! DWC3 (DesignWare USB3 Controller) 驱动
//!
//! DWC3 是一个 USB3 DRD (Dual Role Device) 控制器，支持 Host 和 Device 模式。
//! 本模块实现 Host 模式驱动，基于 xHCI 规范。

use core::ops::{Deref, DerefMut};

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::{boxed::Box, collections::BTreeMap};
use dma_api::{DArray, DmaDirection};
use futures::FutureExt;
use futures::future::BoxFuture;
use tock_registers::interfaces::*;
pub use usb_if::DrMode;
use usb_if::Speed;

use crate::backend::ty::Event;
use crate::backend::{
    kmod::{hub::HubOp, kcore::CoreOp, xhci::Xhci},
    ty::{DeviceOp, EventHandlerOp},
};
use crate::osal::Kernel;
use crate::{DeviceAddressInfo, KernelOp, Mmio};
use reg::GUSB2PHYCFG;
use {
    event::EventBuffer,
    reg::{GCTL, GHWPARAMS0, GHWPARAMS1, GHWPARAMS3, GHWPARAMS4, GUCTL1},
    udphy::Udphy,
};

use crate::err::{Result, USBError};
use reg::GEVNTSIZ;

use usb2phy::Usb2Phy;
pub use usb2phy::Usb2PhyParam;

/// USB PHY 接口模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsbPhyInterfaceMode {
    /// 未知模式
    #[default]
    Unknown,
    /// UTMI 8-bit 接口
    Utmi,
    /// UTMI 16-bit 接口 (UTMIW)
    UtmiWide,
}

pub mod grf;
// pub mod phy;
mod consts;
mod event;
mod reg;
mod udphy;
pub mod usb2phy;

// pub use phy::{UsbDpMode, UsbDpPhy, UsbDpPhyConfig};
use consts::*;
use reg::Dwc3Regs;
pub use udphy::UdphyParam;
// pub use usb2phy::Usb2Phy;

/// CRU (Clock and Reset Unit)
pub trait CruOp: Sync + Send + 'static {
    fn reset_assert(&self, id: u64);
    fn reset_deassert(&self, id: u64);
}

pub struct DwcNewParams<'a, C: CruOp> {
    pub ctrl: Mmio,
    pub phy: Mmio,
    pub phy_param: UdphyParam<'a>,
    pub usb2_phy_param: Usb2PhyParam<'a>,
    pub cru: C,
    pub rst_list: &'a [(&'a str, u64)],
    pub params: DwcParams,
    pub kernel: &'static dyn KernelOp,
}

#[derive(Debug, Default, Clone)]
pub struct DwcParams {
    pub dr_mode: DrMode,
    pub max_speed: Speed,
    pub hsphy_mode: UsbPhyInterfaceMode,
    pub delayed_status: bool,
    pub ep0_bounced: bool,
    pub ep0_expect_in: bool,
    pub has_hibernation: bool,
    pub has_lpm_erratum: bool,
    pub is_utmi_l1_suspend: bool,
    pub is_selfpowered: bool,
    pub is_fpga: bool,
    pub needs_fifo_resize: bool,
    pub pullups_connected: bool,
    pub resize_fifos: bool,
    pub setup_packet_pending: bool,
    pub start_config_issued: bool,
    pub three_stage_setup: bool,
    pub disable_scramble_quirk: bool,
    pub u2exit_lfps_quirk: bool,
    pub u2ss_inp3_quirk: bool,
    pub req_p1p2p3_quirk: bool,
    pub del_p1p2p3_quirk: bool,
    pub del_phy_power_chg_quirk: bool,
    pub lfps_filter_quirk: bool,
    pub rx_detect_poll_quirk: bool,
    pub dis_u3_susphy_quirk: bool,
    pub dis_u2_susphy_quirk: bool,
    pub dis_u1u2_quirk: bool,
    pub dis_enblslpm_quirk: bool,
    pub dis_u2_freeclk_exists_quirk: bool,
    pub tx_de_emphasis_quirk: bool,
    pub tx_de_emphasis: u8,        // 2 bits
    pub usb2_phyif_utmi_width: u8, // 5 bits
}

/// DWC3 控制器
///
/// DWC3 实际上是 xHCI 主机控制器的封装。在 Host 模式下，
/// DWC3 的 xHCI 寄存器区域 (0x0000 - 0x7fff) 包含标准 xHCI 寄存器，
/// 全局寄存器区域 (0xc100 - 0xcfff) 包含 DWC3 特定配置。
pub struct Dwc {
    xhci: Xhci,
    usb3_phy: Udphy,
    usb2_phy: Usb2Phy,
    dwc_regs: Dwc3Regs,
    cru: Arc<dyn CruOp>,
    rsts: BTreeMap<String, u64>,
    ev_buffs: Vec<EventBuffer>,
    revistion: u32,
    nr_scratch: u32,
    params: DwcParams,
    scratchbuf: Option<DArray<u8>>,
}

impl Dwc {
    pub fn new(mut params: DwcNewParams<'_, impl CruOp>) -> Result<Self> {
        let mmio_base = params.ctrl.as_ptr() as usize;
        params.params.max_speed = Speed::Full;
        let cru = Arc::new(params.cru);
        let xhci = Xhci::new(params.ctrl, params.kernel)?;

        let phy = Udphy::new(params.phy, cru.clone(), params.phy_param);
        let usb2_phy = Usb2Phy::new(cru.clone(), params.usb2_phy_param, xhci.kernel().clone());

        let dwc_regs = unsafe { Dwc3Regs::new(mmio_base) };

        let mut rsts = BTreeMap::new();
        for &(name, id) in params.rst_list.iter() {
            rsts.insert(String::from(name), id);
        }

        Ok(Self {
            xhci,
            dwc_regs,
            usb3_phy: phy,
            usb2_phy,
            cru,
            rsts,
            ev_buffs: vec![],
            revistion: 0,
            nr_scratch: 0,
            params: params.params,
            scratchbuf: None,
        })
    }

    async fn dwc3_init(&mut self) -> Result<()> {
        self.alloc_event_buffers(DWC3_EVENT_BUFFERS_SIZE)?;
        self.core_init().await?;
        self.event_buffers_setup();

        Ok(())
    }

    fn alloc_event_buffers(&mut self, len: usize) -> Result<()> {
        let num_buffs = self
            .dwc_regs
            .globals()
            .ghwparams1
            .read(GHWPARAMS1::NUM_EVENT_BUFFERS);
        debug!("Allocating {} event buffers", num_buffs);
        for _ in 0..num_buffs {
            let ev_buff = EventBuffer::new(len, self.kernel())?;
            self.ev_buffs.push(ev_buff);
        }
        Ok(())
    }

    fn event_buffers_setup(&mut self) {
        info!("DWC3: Setting up event buffers");

        let regs = self.dwc_regs.globals();

        for (i, ev_buff) in self.ev_buffs.iter().enumerate() {
            if i >= regs.gevnt.len() {
                warn!("DWC3: Invalid event buffer index {}", i);
                break;
            }

            let dma_addr = ev_buff.dma_addr();
            let length = ev_buff.buffer.len();

            debug!(
                "DWC3: Event buffer {} - DMA addr: {:#x}, length: {}",
                i, dma_addr, length
            );

            // 使用 gevnt 数组访问事件缓冲区寄存器
            regs.gevnt[i].adrlo.set((dma_addr & 0xffffffff) as u32);
            regs.gevnt[i].adrhi.set((dma_addr >> 32) as u32);

            // 关键修复：设置 GEVNTSIZ 时必须清除 INTMASK 位（bit 31）
            // INTMASK = 0: 使能事件中断；INTMASK = 1: 屏蔽事件中断
            regs.gevnt[i]
                .size
                .modify(GEVNTSIZ::INTMASK::Unmasked + GEVNTSIZ::SIZE.val(length as _));

            regs.gevnt[i].count.set(0);

            debug!(
                "DWC3: GEVNTSIZ[{}] = {:?} (INTMASK cleared, SIZE={})",
                i,
                regs.gevnt[i].size.debug(),
                length
            );
        }

        debug!("DWC3: Event buffers setup completed");
    }

    async fn core_init(&mut self) -> Result<()> {
        self.revistion = self.dwc_regs.read_revision() as _;
        if self.revistion != 0x55330000 {
            Err(anyhow!(
                "Unsupported DWC3 revision: 0x{:08x}",
                self.revistion
            ))?;
        }
        self.revistion += self.dwc_regs.read_product_id();
        debug!("DWC3: Detected revision 0x{:08x}", self.revistion);

        if let Some(GHWPARAMS3::SSPHY_IFC::Value::Disabled) = self
            .dwc_regs
            .globals()
            .ghwparams3
            .read_as_enum(GHWPARAMS3::SSPHY_IFC)
            && self.max_speed == Speed::SuperSpeed
        {
            self.max_speed = Speed::High;
        }

        debug!("DWC3: Max speed {:?}", self.max_speed);

        self.dwc_regs.device_soft_reset().await;

        // PHY 软复位（包含 PHY 复位和核心复位）
        info!("DWC3: Starting core soft reset (includes PHY soft reset)");
        self.dwc_regs.core_soft_reset(self.kernel()).await;

        // **关键调试：检查 PHY 软复位后的寄存器状态**
        let gusb3_val = self.dwc_regs.globals().gusb3pipectl0.extract();
        let gusb2_val = self.dwc_regs.globals().gusb2phycfg0.extract();
        info!(
            "DWC3: After core_soft_reset - GUSB3PIPECTL={:#010x}, GUSB2PHYCFG={:#010x}",
            gusb3_val.get(),
            gusb2_val.get()
        );

        // **关键修复：在初始化开始时清除 suspendusb20 位（RK3588 TRM 要求）**
        // TRM 明确说明：如果此位为 1，应用程序必须在 power-on reset 后清除此位
        info!("DWC3: Clearing suspendusb20 bit (TRM requirement)");
        self.dwc_regs
            .globals()
            .gusb2phycfg0
            .modify(GUSB2PHYCFG::SUSPHY::Disable);
        if self.revistion >= DWC3_REVISION_250A {
            debug!("DWC3: Revision 250A or later detected");

            if matches!(self.max_speed, Speed::Full | Speed::High) {
                self.dwc_regs
                    .globals()
                    .guctl1
                    .modify(GUCTL1::DEV_FORCE_20_CLK_FOR_30_CLK::Enable);
            }
        }

        let mut reg = self.dwc_regs.globals().gctl.extract();
        reg.modify(GCTL::SCALEDOWN::None);

        match self
            .dwc_regs
            .globals()
            .ghwparams1
            .read_as_enum(GHWPARAMS1::EN_PWROPT)
        {
            Some(GHWPARAMS1::EN_PWROPT::Value::Clock) => {
                if (DWC3_REVISION_210A..=DWC3_REVISION_250A).contains(&self.revistion) {
                    reg.modify(GCTL::DSBLCLKGTNG::Enable + GCTL::SOFITPSYNC::Enable);
                } else {
                    reg.modify(GCTL::DSBLCLKGTNG::Disable);
                }
            }
            Some(GHWPARAMS1::EN_PWROPT::Value::Hibernation) => {
                self.nr_scratch = self
                    .dwc_regs
                    .globals()
                    .ghwparams4
                    .read(GHWPARAMS4::HIBER_SCRATCHBUFS) as _;

                reg.modify(GCTL::GBLHIBERNATIONEN::Enable);
            }
            _ => {
                debug!("No power optimization available");
            }
        }
        reg.modify(GCTL::DISSCRAMBLE::Disable);

        if self.u2exit_lfps_quirk {
            reg.modify(GCTL::U2EXIT_LFPS::Enable);
        }
        /*
         * WORKAROUND: DWC3 revisions <1.90a have a bug
         * where the device can fail to connect at SuperSpeed
         * and falls back to high-speed mode which causes
         * the device to enter a Connect/Disconnect loop
         */
        if self.revistion < DWC3_REVISION_190A {
            debug!("Applying DWC3 <1.90a SuperSpeed connect workaround");
            reg.modify(GCTL::U2RSTECN::Enable);
        }

        // core_num_eps

        self.dwc_regs.globals().gctl.set(reg.get());

        self.phy_setup().await?;

        self.alloc_scratch_buffers()?;

        self.setup_scratch_buffers();

        self.core_init_mode()?;

        Ok(())
    }

    /// 配置 USB2 High-Speed PHY 接口模式
    ///
    /// 根据 hsphy_mode 配置 PHY 接口：
    /// - Utmi: 8-bit UTMI 接口 (USBTRDTIM=9, PHYIF=0)
    /// - UtmiWide: 16-bit UTMI 接口 (USBTRDTIM=5, PHYIF=1)
    fn hsphy_mode_setup(&mut self) {
        use reg::GUSB2PHYCFG;

        match self.hsphy_mode {
            UsbPhyInterfaceMode::Utmi => {
                // 8-bit UTMI 接口
                self.dwc_regs.globals().gusb2phycfg0.modify(
                    GUSB2PHYCFG::PHYIF.val(0) + // UTMI_PHYIF_8_BIT
                    GUSB2PHYCFG::USBTRDTIM.val(9), // USBTRDTIM_UTMI_8_BIT
                );
                debug!("DWC3: HS PHY configured as UTMI 8-bit");
            }
            UsbPhyInterfaceMode::UtmiWide => {
                // 16-bit UTMI 接口
                self.dwc_regs.globals().gusb2phycfg0.modify(
                    GUSB2PHYCFG::PHYIF.val(1) + // UTMI_PHYIF_16_BIT
                    GUSB2PHYCFG::USBTRDTIM.val(5), // USBTRDTIM_UTMI_16_BIT
                );
                debug!("DWC3: HS PHY configured as UTMI 16-bit");
            }
            UsbPhyInterfaceMode::Unknown => {
                debug!("DWC3: HS PHY mode unknown, using default configuration");
            }
        }
    }

    async fn phy_setup(&mut self) -> Result<()> {
        use reg::{GUSB2PHYCFG, GUSB3PIPECTL};

        info!("DWC3: Configuring PHY");

        let is_mode_drd = matches!(
            self.dwc_regs
                .globals()
                .ghwparams0
                .read_as_enum(GHWPARAMS0::MODE),
            Some(GHWPARAMS0::MODE::Value::DRD)
        );

        // === USB3 PHY 配置 ===
        // **关键：读取当前寄存器值（保留硬件状态）**
        let gusb3_init = self.dwc_regs.globals().gusb3pipectl0.extract();
        info!(
            "DWC3: Initial GUSB3PIPECTL = {:#010x} before config",
            gusb3_init.get()
        );

        let mut gusb3 = self.dwc_regs.globals().gusb3pipectl0.extract();

        /*
         * Above 1.94a, it is recommended to set DWC3_GUSB3PIPECTL_SUSPHY
         * to '0' during coreConsultant configuration. So default value
         * will be '0' when the core is reset. Application needs to set it
         * to '1' after the core initialization is completed.
         */
        if self.revistion > DWC3_REVISION_194A {
            gusb3.modify(GUSB3PIPECTL::SUSPHY::Enable);
        }

        if is_mode_drd {
            gusb3.modify(GUSB3PIPECTL::SUSPHY::Disable);
        }

        if self.u2ss_inp3_quirk {
            gusb3.modify(GUSB3PIPECTL::U2SSINP3OK::Enable);
        }

        if self.req_p1p2p3_quirk {
            gusb3.modify(GUSB3PIPECTL::REQP0P1P2P3::Yes);
        }

        if self.del_p1p2p3_quirk {
            gusb3.modify(GUSB3PIPECTL::DEP1P2P3::Enable);
        }

        if self.del_phy_power_chg_quirk {
            gusb3.modify(GUSB3PIPECTL::DEPOCHANGE::Enable);
        }

        if self.lfps_filter_quirk {
            gusb3.modify(GUSB3PIPECTL::LFPSFILT::Enable);
        }

        if self.rx_detect_poll_quirk {
            gusb3.modify(GUSB3PIPECTL::RX_DETOPOLL::Enable);
        }

        if self.tx_de_emphasis_quirk {
            gusb3.modify(GUSB3PIPECTL::TX_DEEPH.val(self.tx_de_emphasis as u32));
        }

        const IS_ROCKCHIP: bool = true;
        /*
         * For some Rockchip SoCs like RK3588, if the USB3 PHY is suspended
         * in U-Boot would cause the PHY initialize abortively in Linux Kernel,
         * so disable the DWC3_GUSB3PIPECTL_SUSPHY feature here to fix it.
         */
        if self.dis_u3_susphy_quirk || IS_ROCKCHIP {
            gusb3.modify(GUSB3PIPECTL::SUSPHY::Disable);
        }

        self.dwc_regs.globals().gusb3pipectl0.set(gusb3.get());

        // 配置 USB2 High-Speed PHY 接口模式
        self.hsphy_mode_setup();

        self.kernel().delay(core::time::Duration::from_millis(100));

        // === USB2 PHY 配置 ===
        // **关键：读取当前寄存器值（保留硬件状态）**
        let gusb2_init = self.dwc_regs.globals().gusb2phycfg0.extract();
        info!(
            "DWC3: Initial GUSB2PHYCFG = {:#010x} before config",
            gusb2_init.get()
        );

        let mut gusb2 = self.dwc_regs.globals().gusb2phycfg0.extract();

        /*
         * Above 1.94a, it is recommended to set DWC3_GUSB2PHYCFG_SUSPHY to
         * '0' during coreConsultant configuration. So default value will
         * be '0' when the core is reset. Application needs to set it to
         * '1' after the core initialization is completed.
         */
        if self.revistion > DWC3_REVISION_194A {
            gusb2.modify(GUSB2PHYCFG::SUSPHY::Enable);
        }

        if is_mode_drd {
            gusb2.modify(GUSB2PHYCFG::SUSPHY::Disable);
        }

        if self.dis_u2_susphy_quirk {
            gusb2.modify(GUSB2PHYCFG::SUSPHY::Disable);
        }

        if self.dis_enblslpm_quirk {
            gusb2.modify(GUSB2PHYCFG::ENBLSLPM::Disable);
        } else {
            gusb2.modify(GUSB2PHYCFG::ENBLSLPM::Enable);
        }

        if self.dis_u2_freeclk_exists_quirk {
            gusb2.modify(GUSB2PHYCFG::U2_FREECLK_EXISTS::No);
        }

        // 注意：PHYIF 和 USBTRDTIM 已在 hsphy_mode_setup() 中配置
        // 不要重复配置，避免覆盖正确的设置

        self.dwc_regs.globals().gusb2phycfg0.set(gusb2.get());

        self.kernel().delay(core::time::Duration::from_millis(100));

        debug!("DWC3: PHY configuration completed");

        Ok(())
    }

    fn alloc_scratch_buffers(&mut self) -> Result<()> {
        if !self.has_hibernation {
            return Ok(());
        }

        if self.nr_scratch == 0 {
            return Ok(());
        }

        let scratch_size = (self.nr_scratch as usize) * DWC3_SCRATCHBUF_SIZE;

        let scratchbuf = self
            .kernel()
            .array_zero_with_align(
                scratch_size,
                self.kernel().page_size(),
                DmaDirection::Bidirectional,
            )
            .map_err(|_| USBError::NoMemory)?;

        // let scratchbuf = DVec::zeros(
        //     self.xhci.dma_mask as _,
        //     scratch_size,
        //     page_size(),
        //     dma_api::Direction::Bidirectional,
        // )
        // .map_err(|_| USBError::NoMemory)?;

        self.scratchbuf = Some(scratchbuf);
        debug!(
            "DWC3: Allocated {} scratch buffers (total {} bytes)",
            self.nr_scratch, scratch_size
        );

        Ok(())
    }

    fn setup_scratch_buffers(&mut self) {
        if let Some(_scratchbuf) = &self.scratchbuf {
            todo!()
        }
    }

    fn core_init_mode(&mut self) -> Result<()> {
        match self.dr_mode {
            DrMode::Host => {
                info!("DWC3: Initializing in HOST mode");
                self.dwc_regs.globals().gctl.modify(GCTL::PRTCAPDIR::Host);
            }
            DrMode::Otg => {
                todo!()
            }
            DrMode::Peripheral => todo!(),
        }

        Ok(())
    }

    /// 输出关键寄存器状态用于调试
    fn dump_registers(&self) {
        use reg::*;

        let regs = self.dwc_regs.globals();

        info!("=== DWC3 寄存器状态 ===");

        // 检查 GCTL
        let gctl = regs.gctl.extract();
        let gctl_val = gctl.get();
        info!("GCTL         = {:#010x}", gctl_val);
        let prtcapdir_val = gctl.read(GCTL::PRTCAPDIR);
        let prtcapdir_str = match prtcapdir_val {
            0 => "Device",
            1 => "Host",
            2 => "OTG",
            3 => "Reserved",
            _ => "Unknown",
        };
        info!("  PRTCAPDIR   = {} ({})", prtcapdir_str, prtcapdir_val);

        // 检查 GUSB3PIPECTL
        let gusb3 = regs.gusb3pipectl0.extract();
        let gusb3_val = gusb3.get();
        info!("GUSB3PIPECTL = {:#010x}", gusb3_val);
        info!("  SUSPHY      = {}", gusb3.is_set(GUSB3PIPECTL::SUSPHY));
        info!("  U2SSINP3OK  = {}", gusb3.is_set(GUSB3PIPECTL::U2SSINP3OK));
        info!(
            "  REQP0P1P2P3 = {}",
            gusb3.is_set(GUSB3PIPECTL::REQP0P1P2P3)
        );
        info!("  DEP1P2P3    = {}", gusb3.is_set(GUSB3PIPECTL::DEP1P2P3));

        // 检查 GUSB2PHYCFG
        let gusb2 = regs.gusb2phycfg0.extract();
        let gusb2_val = gusb2.get();
        info!("GUSB2PHYCFG  = {:#010x}", gusb2_val);
        info!("  SUSPHY      = {}", gusb2.is_set(GUSB2PHYCFG::SUSPHY));
        info!("  ENBLSLPM    = {}", gusb2.is_set(GUSB2PHYCFG::ENBLSLPM));
        let phyif = gusb2.read(GUSB2PHYCFG::PHYIF);
        info!(
            "  PHYIF       = {} ({}-bit)",
            phyif,
            if phyif == 0 { 8 } else { 16 }
        );
        let usbtrdtim = gusb2.read(GUSB2PHYCFG::USBTRDTIM);
        info!("  USBTRDTIM   = {}", usbtrdtim);

        // 检查 GHWPARAMS
        let hwparams0 = regs.ghwparams0.extract();
        info!("GHWPARAMS0   = {:#010x}", hwparams0.get());
        let mode_val = hwparams0.read(GHWPARAMS0::MODE);
        let mode_str = match mode_val {
            0 => "Gadget",
            1 => "Host",
            2 => "DRD",
            3 => "Reserved",
            _ => "Unknown",
        };
        info!("  MODE        = {} ({})", mode_str, mode_val);

        let hwparams1 = regs.ghwparams1.extract();
        let num_event_buffers = hwparams1.read(GHWPARAMS1::NUM_EVENT_BUFFERS);
        info!("GHWPARAMS1   = {:#010x}", hwparams1.get());
        info!("  NUM_EVENT_BUFFERS = {}", num_event_buffers);

        info!("======================");
    }
    /// 初始化 DWC3 控制器
    ///
    /// ## 初始化顺序说明
    ///
    /// 在 HOST 模式下，必须按照以下顺序初始化：
    /// 1. USBDP PHY 硬件初始化（时钟、复位、PLL）
    /// 2. DWC3 全局配置（GCTL、HOST 模式）
    /// 3. **xHCI 主机控制器初始化**（执行 HCRST 复位）
    /// 4. DWC3 PHY 配置寄存器（GUSB3PIPECTL、GUSB2PHYCFG）
    ///
    /// **关键点**：DWC3 PHY 配置寄存器必须在 xHCI 执行 HCRST **之后**才能访问，
    /// 因为 HCRST 会复位并使能 host block 的 PHY 接口。
    async fn _init(&mut self) -> Result {
        info!("DWC3: Starting controller initialization");

        /*
         * It must hold whole USB3.0 OTG controller in resetting to hold pipe
         * power state in P2 before initializing TypeC PHY on RK3399 platform.
         */
        for &id in self.rsts.values() {
            self.cru.reset_assert(id);
        }

        self.kernel().delay(core::time::Duration::from_millis(1));
        // 初始化 USB2 PHY（需要在 xHCI HCRST 之前）
        self.usb2_phy.setup().await?;

        let kernel = self.kernel().clone();
        self.usb3_phy.setup(&kernel).await?;

        for &id in self.rsts.values() {
            self.cru.reset_deassert(id);
        }

        self.dwc3_init().await?;

        self.xhci.init().await?;

        // 输出关键寄存器状态用于调试
        self.dump_registers();

        Ok(())
    }
}

// impl BackendOp for Dwc {
//     fn init(&mut self) -> futures::future::BoxFuture<'_, Result<()>> {
//         self._init().boxed()
//     }

//     fn device_list(
//         &mut self,
//     ) -> futures::future::BoxFuture<'_, Result<Vec<Box<dyn super::ty::DeviceInfoOp>>>> {
//         self.xhci.device_list()
//     }

//     fn open_device<'a>(
//         &'a mut self,
//         dev: &'a dyn super::ty::DeviceInfoOp,
//     ) -> futures::future::LocalBoxFuture<'a, Result<Box<dyn super::ty::DeviceOp>>> {
//         self.xhci.open_device(dev)
//     }

//     fn create_event_handler(&mut self) -> Box<dyn super::ty::EventHandlerOp> {
//         Box::new(DwcEventHandler {
//             xhci: self.xhci.create_event_handler(),
//             _dwc: self.dwc_regs.clone(),
//         })
//     }
// }

impl CoreOp for Dwc {
    fn init(&mut self) -> BoxFuture<'_, Result<()>> {
        self._init().boxed()
    }

    fn root_hub(&mut self) -> Box<dyn HubOp> {
        self.xhci.root_hub()
    }

    fn create_event_handler(&mut self) -> Box<dyn EventHandlerOp> {
        Box::new(DwcEventHandler {
            xhci: self.xhci.create_event_handler(),
            _dwc: self.dwc_regs.clone(),
        })
    }

    fn new_addressed_device<'a>(
        &'a mut self,
        addr: DeviceAddressInfo,
    ) -> BoxFuture<'a, Result<Box<dyn DeviceOp>>> {
        self.xhci.new_addressed_device(addr)
    }

    fn kernel(&self) -> &Kernel {
        self.xhci.kernel()
    }
}

impl Deref for Dwc {
    type Target = DwcParams;

    fn deref(&self) -> &Self::Target {
        &self.params
    }
}

impl DerefMut for Dwc {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.params
    }
}

pub struct DwcEventHandler {
    xhci: Box<dyn EventHandlerOp>,
    _dwc: Dwc3Regs,
}
impl EventHandlerOp for DwcEventHandler {
    fn handle_event(&self) -> Event {
        // let cnt = self.dwc.globals().gevnt[0].count.get();
        // debug!("DWC3 Event Handler: GEVNT[0] COUNT = {}", cnt);
        // self.dwc.globals().gevnt[0].count.set(0);

        self.xhci.handle_event()
    }
}
