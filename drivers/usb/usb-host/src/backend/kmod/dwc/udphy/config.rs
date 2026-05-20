#[derive(Clone)]
#[repr(C)]
pub struct UdphyGrfReg {
    pub offset: u32,
    pub bitend: u32,
    pub bitstart: u32,
    pub disable: u32,
    pub enable: u32,
}

impl UdphyGrfReg {
    pub const fn new(offset: u32, bitend: u32, bitstart: u32, disable: u32, enable: u32) -> Self {
        Self {
            offset,
            bitend,
            bitstart,
            disable,
            enable,
        }
    }

    pub const fn default() -> Self {
        Self {
            offset: 0,
            bitend: 0,
            bitstart: 0,
            disable: 0,
            enable: 0,
        }
    }
}

#[derive(Clone)]
pub struct UdphyCfg {
    pub rst_list: &'static [&'static str],
    pub grf: UdphyGrfCfg,
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct UdphyGrfCfg {
    /// Bvalid PHY 控制（设备模式使用）
    pub bvalid_phy_con: UdphyGrfReg,
    /// Bvalid GRF 控制（设备模式使用）
    pub bvalid_grf_con: UdphyGrfReg,

    pub usb3otg0_cfg: UdphyGrfReg,
    pub usb3otg1_cfg: UdphyGrfReg,

    pub low_pwrn: UdphyGrfReg,
    pub rx_lfps: UdphyGrfReg,
}

pub const RK3588_UDPHY_CFGS: UdphyCfg = UdphyCfg {
    rst_list: &["init", "cmn", "lane", "pcs_apb", "pma_apb"],
    grf: UdphyGrfCfg {
        bvalid_phy_con: UdphyGrfReg {
            offset: 0x0008,
            bitend: 1,
            bitstart: 0,
            disable: 2,
            enable: 3,
        },
        bvalid_grf_con: UdphyGrfReg {
            offset: 0x0010,
            bitend: 3,
            bitstart: 2,
            disable: 2,
            enable: 3,
        },
        usb3otg0_cfg: UdphyGrfReg {
            offset: 0x001c,
            bitend: 15,
            bitstart: 0,
            disable: 0x1100,
            enable: 0x0188,
        },
        usb3otg1_cfg: UdphyGrfReg {
            offset: 0x0034,
            bitend: 15,
            bitstart: 0,
            disable: 0x1100,
            enable: 0x0188,
        },
        low_pwrn: UdphyGrfReg {
            offset: 0x0004,
            bitend: 13,
            bitstart: 13,
            disable: 0,
            enable: 1,
        },
        rx_lfps: UdphyGrfReg {
            offset: 0x0004,
            bitend: 14,
            bitstart: 14,
            disable: 0,
            enable: 1,
        },
    },
};
