use log::warn;

use crate::{ChipVersion, Error, Result, Rtl8125};

const OCP_STD_PHY_BASE: u32 = 0xa400;
const EEE_TXIDLE_TIMER_VALUE: u16 = 1500 + 14 + 0x20;
const CSI_PCIE_CTRL: u32 = 0x070c;
const CSI_PCIE_ZRXDC_NONCOMPL: u32 = 1 << 20;
const CSI_L1_ENTRY_LATENCY: u32 = 0x0719;
const CSI_L1_ENTRY_LATENCY_DEFAULT: u8 = 0x27;

const MII_BMCR: u32 = 0x00;
const MII_CTRL1000: u32 = 0x09;

const BMCR_ANRESTART: u16 = 0x0200;
const BMCR_ANENABLE: u16 = 0x1000;
const BMCR_AUTONEG_MASK: u16 = BMCR_ANENABLE | BMCR_ANRESTART;

const ADVERTISE_1000HALF: u16 = 0x0100;
const ADVERTISE_1000FULL: u16 = 0x0200;
const ADVERTISE_1000_MASK: u16 = ADVERTISE_1000HALF | ADVERTISE_1000FULL;

trait PhyRegisterAccess {
    fn read_phy(&mut self, reg: u32) -> Result<u16>;

    fn write_phy(&mut self, reg: u32, value: u16) -> Result<()>;
}

impl Rtl8125 {
    pub(crate) fn hw_init_8125(&self) -> Result<()> {
        self.enable_rxdv_gate();
        self.regs.disable_tx_rx();
        spin_delay(10_000);
        self.regs.clear_now_is_oob();

        self.mac_ocp_modify(0xe8de, 1 << 14, 0)?;
        self.wait_link_list_ready();
        self.mac_ocp_write(0xc0aa, 0x07d0)?;
        self.mac_ocp_write(0xc0a6, 0x0150)?;
        self.mac_ocp_write(0xc01e, 0x5555)?;
        self.wait_link_list_ready();
        Ok(())
    }

    pub(crate) fn hw_start_8125(&mut self) -> Result<()> {
        for offset in (0x0a00..0x0b00).step_by(4) {
            self.regs.write_vendor_u32(offset, 0);
        }

        self.set_aspm_clkreq(false)?;
        match self.chip {
            ChipVersion::Rtl8125A => self.ephy_init(&RTL8125A_EPHY)?,
            ChipVersion::Rtl8125B | ChipVersion::Unknown(_) => self.ephy_init(&RTL8125B_EPHY)?,
        }

        self.hw_start_8125_common()
    }

    fn hw_start_8125_common(&self) -> Result<()> {
        self.regs.clear_ready_to_l23();
        self.regs.write_vendor_u16(0x0382, 0x221b);
        self.regs.write_vendor_u8(0x4500, 0);
        self.regs.write_vendor_u16(0x4800, 0);
        self.mac_ocp_modify(0xd40a, 0x0010, 0)?;
        self.regs.clear_speed_down();
        self.mac_ocp_write(0xc140, 0xffff)?;
        self.mac_ocp_write(0xc142, 0xffff)?;
        self.mac_ocp_modify(0xd3e2, 0x0fff, 0x03a9)?;
        self.mac_ocp_modify(0xd3e4, 0x00ff, 0)?;
        self.mac_ocp_modify(0xe860, 0, 0x0080)?;
        self.mac_ocp_modify(0xeb58, 0x0001, 0)?;
        self.disable_zrxdc_noncompliance_timeout();
        self.set_l1_entry_latency(CSI_L1_ENTRY_LATENCY_DEFAULT);

        if self.chip == ChipVersion::Rtl8125B {
            self.mac_ocp_modify(0xe614, 0x0700, 0x0200)?;
            self.mac_ocp_modify(0xe63e, 0x0c30, 0)?;
        } else {
            self.mac_ocp_modify(0xe614, 0x0700, 0x0400)?;
            self.mac_ocp_modify(0xe63e, 0x0c30, 0x0020)?;
        }

        self.mac_ocp_modify(0xc0b4, 0, 0x000c)?;
        self.mac_ocp_modify(0xeb6a, 0x00ff, 0x0033)?;
        self.mac_ocp_modify(0xeb50, 0x03e0, 0x0040)?;
        self.mac_ocp_modify(0xe056, 0x00f0, 0x0030)?;
        self.mac_ocp_modify(0xe040, 0x1000, 0)?;
        self.mac_ocp_modify(0xea1c, 0x0003, 0x0001)?;
        self.mac_ocp_modify(0xe0c0, 0x4f0f, 0x4403)?;
        self.mac_ocp_modify(0xe052, 0x0080, 0x0068)?;
        self.mac_ocp_modify(0xd430, 0x0fff, 0x047f)?;
        self.mac_ocp_modify(0xea1c, 0x0004, 0)?;
        self.mac_ocp_modify(0xeb54, 0, 0x0001)?;
        spin_delay(100);
        self.mac_ocp_modify(0xeb54, 0x0001, 0)?;
        self.regs.clear_vendor_u16_bits(0x1880, 0x0030);
        self.mac_ocp_write(0xe098, 0xc302)?;
        self.wait_mac_ocp_e00e_low();
        self.config_eee_mac()?;
        self.disable_rxdv_gate();
        Ok(())
    }

    fn disable_zrxdc_noncompliance_timeout(&self) {
        let Some(value) = self.regs.csi_read(CSI_PCIE_CTRL) else {
            warn!("RTL8125: failed to read CSI {CSI_PCIE_CTRL:#x}");
            return;
        };
        if !self
            .regs
            .csi_write(CSI_PCIE_CTRL, value & !CSI_PCIE_ZRXDC_NONCOMPL)
        {
            warn!("RTL8125: failed to write CSI {CSI_PCIE_CTRL:#x}");
        }
    }

    fn set_l1_entry_latency(&self, value: u8) {
        let addr = CSI_L1_ENTRY_LATENCY & !0x3;
        let shift = (CSI_L1_ENTRY_LATENCY & 0x3) * 8;
        let Some(old) = self.regs.csi_read(addr) else {
            warn!("RTL8125: failed to read CSI {addr:#x}");
            return;
        };
        let new = (old & !(0xff << shift)) | (u32::from(value) << shift);
        if !self.regs.csi_write(addr, new) {
            warn!("RTL8125: failed to write CSI {addr:#x}");
        }
    }

    pub(crate) fn maybe_start_queues(&mut self) {
        crate::queue::try_start_queues(self.regs, self.dma.dma_mask(), &self.queue_start);
    }

    fn enable_rxdv_gate(&self) {
        self.regs.enable_rxdv_gate();
        spin_delay(2_000);
        self.wait_rxtx_empty();
    }

    fn disable_rxdv_gate(&self) {
        self.regs.disable_rxdv_gate();
    }

    fn wait_rxtx_empty(&self) {
        for _ in 0..4_200 {
            if self.regs.rxtx_empty() {
                return;
            }
            core::hint::spin_loop();
        }
        warn!("RTL8125: timed out waiting for RX/TX FIFO empty");
    }

    fn wait_mac_ocp_e00e_low(&self) {
        for _ in 0..10 {
            match self.mac_ocp_read(0xe00e) {
                Ok(value) if value & (1 << 13) == 0 => return,
                Ok(_) => spin_delay(1_000),
                Err(err) => {
                    warn!("RTL8125: failed to read MAC OCP 0xe00e: {err:?}");
                    return;
                }
            }
        }
        warn!("RTL8125: timed out waiting for MAC OCP 0xe00e bit 13 to clear");
    }

    fn set_aspm_clkreq(&self, enable: bool) -> Result<()> {
        if enable {
            self.regs.set_aspm_clkreq(true);
            self.mac_ocp_modify(0xe094, 0xff00, 0)?;
            self.mac_ocp_modify(0xe092, 0x00ff, 1 << 2)?;
        } else {
            self.mac_ocp_modify(0xe092, 0x00ff, 0)?;
            self.regs.set_aspm_clkreq(false);
        }
        spin_delay(100);
        Ok(())
    }

    fn config_eee_mac(&self) -> Result<()> {
        if self.chip == ChipVersion::Rtl8125B {
            self.regs.write_eee_txidle_timer(EEE_TXIDLE_TIMER_VALUE);
        } else {
            self.mac_ocp_modify(0xeb62, 0, (1 << 2) | (1 << 1))?;
        }
        self.mac_ocp_modify(0xe040, 0, (1 << 1) | 1)
    }

    pub(crate) fn ack_events(&self, bits: u32) {
        self.regs.write_interrupt_status(bits);
    }

    fn mac_ocp_write(&self, reg: u32, data: u16) -> Result<()> {
        validate_ocp_reg(reg)?;
        self.regs.start_mac_ocp_write(reg, data);
        Ok(())
    }

    fn mac_ocp_read(&self, reg: u32) -> Result<u16> {
        validate_ocp_reg(reg)?;
        self.regs.start_mac_ocp_read(reg);
        Ok(self.regs.read_mac_ocp_data())
    }

    fn mac_ocp_modify(&self, reg: u32, mask: u16, set: u16) -> Result<()> {
        let data = self.mac_ocp_read(reg)?;
        self.mac_ocp_write(reg, (data & !mask) | set)
    }

    fn ephy_read(&self, reg: u32) -> Result<u16> {
        self.regs.start_ephy_read(reg);
        for _ in 0..1_000 {
            if self.regs.ephy_ready() {
                return Ok(self.regs.read_ephy_data());
            }
            core::hint::spin_loop();
        }
        Err(Error::HardwareTimeout {
            operation: "EPHY read",
        })
    }

    fn ephy_write(&self, reg: u32, value: u16) -> Result<()> {
        self.regs.start_ephy_write(reg, value);
        for _ in 0..1_000 {
            if !self.regs.ephy_ready() {
                spin_delay(1_000);
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::HardwareTimeout {
            operation: "EPHY write",
        })
    }

    fn ephy_init(&self, entries: &[EphyInfo]) -> Result<()> {
        for entry in entries {
            let value = (self.ephy_read(entry.offset.into())? & !entry.mask) | entry.bits;
            self.ephy_write(entry.offset.into(), value)?;
        }
        Ok(())
    }

    fn phy_ocp_write(&self, reg: u32, data: u16) -> Result<()> {
        validate_ocp_reg(reg)?;
        self.regs.start_phy_ocp_write(reg, data);
        for _ in 0..1_000 {
            if !self.regs.phy_ocp_busy() {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::HardwareTimeout {
            operation: "PHY OCP write",
        })
    }

    fn phy_ocp_read(&self, reg: u32) -> Result<u16> {
        validate_ocp_reg(reg)?;
        self.regs.start_phy_ocp_read(reg);
        for _ in 0..1_000 {
            if self.regs.phy_ocp_busy() {
                return Ok(self.regs.read_phy_ocp_data());
            }
            core::hint::spin_loop();
        }
        Err(Error::HardwareTimeout {
            operation: "PHY OCP read",
        })
    }

    fn phy_reg_addr(&self, reg: u32) -> u32 {
        let reg = if self.phy_ocp_base == OCP_STD_PHY_BASE {
            reg
        } else {
            reg.saturating_sub(0x10)
        };
        self.phy_ocp_base + reg * 2
    }

    fn phy_write(&mut self, reg: u32, value: u16) -> Result<()> {
        if reg == 0x1f {
            self.phy_ocp_base = if value == 0 {
                OCP_STD_PHY_BASE
            } else {
                u32::from(value) << 4
            };
            return Ok(());
        }

        self.phy_ocp_write(self.phy_reg_addr(reg), value)
    }

    fn phy_read(&self, reg: u32) -> Result<u16> {
        if reg == 0x1f {
            return Ok(if self.phy_ocp_base == OCP_STD_PHY_BASE {
                0
            } else {
                (self.phy_ocp_base >> 4) as u16
            });
        }

        self.phy_ocp_read(self.phy_reg_addr(reg))
    }

    fn phy_modify(&mut self, reg: u32, mask: u16, set: u16) -> Result<()> {
        let data = self.phy_read(reg)?;
        self.phy_write(reg, (data & !mask) | set)
    }

    fn phy_write_paged(&mut self, page: u16, reg: u32, value: u16) -> Result<()> {
        let old_page = self.phy_read(0x1f)?;
        self.phy_write(0x1f, page)?;
        self.phy_write(reg, value)?;
        self.phy_write(0x1f, old_page)
    }

    fn phy_modify_paged(&mut self, page: u16, reg: u32, mask: u16, set: u16) -> Result<()> {
        let old_page = self.phy_read(0x1f)?;
        self.phy_write(0x1f, page)?;
        self.phy_modify(reg, mask, set)?;
        self.phy_write(0x1f, old_page)
    }

    fn phy_param(&mut self, param: u16, mask: u16, set: u16) -> Result<()> {
        let old_page = self.phy_read(0x1f)?;
        self.phy_write(0x1f, 0x0a43)?;
        self.phy_write(0x13, param)?;
        self.phy_modify(0x14, mask, set)?;
        self.phy_write(0x1f, old_page)
    }

    fn config_eee_phy_8125a(&mut self) -> Result<()> {
        self.phy_modify_paged(0x0a43, 0x11, 0, 1 << 4)?;
        self.phy_modify_paged(0x0a4a, 0x11, 0, 1 << 9)?;
        self.phy_modify_paged(0x0a42, 0x14, 0, 1 << 7)?;
        self.phy_modify_paged(0x0a6d, 0x12, 1, 0)?;
        self.phy_modify_paged(0x0a6d, 0x14, 1 << 4, 0)
    }

    fn config_eee_phy_8125b(&mut self) -> Result<()> {
        self.phy_modify_paged(0x0a6d, 0x12, 1, 0)?;
        self.phy_modify_paged(0x0a6d, 0x14, 1 << 4, 0)?;
        self.phy_modify_paged(0x0a42, 0x14, 1 << 7, 0)?;
        self.phy_modify_paged(0x0a4a, 0x11, 1 << 9, 0)
    }

    pub(crate) fn hw_phy_config(&mut self) -> Result<()> {
        match self.chip {
            ChipVersion::Rtl8125A => self.hw_phy_config_8125a(),
            ChipVersion::Rtl8125B | ChipVersion::Unknown(_) => self.hw_phy_config_8125b(),
        }?;
        configure_default_copper_autoneg(self)
    }

    fn hw_phy_config_8125a(&mut self) -> Result<()> {
        self.phy_modify_paged(0x0ad4, 0x17, 0, 0x0010)?;
        self.phy_modify_paged(0x0ad1, 0x13, 0x03ff, 0x03ff)?;
        self.phy_modify_paged(0x0ad3, 0x11, 0x003f, 0x0006)?;
        self.phy_modify_paged(0x0ac0, 0x14, 0x1100, 0)?;
        self.phy_modify_paged(0x0acc, 0x10, 0x0003, 0x0002)?;
        self.phy_modify_paged(0x0ad4, 0x10, 0x00e7, 0x0044)?;
        self.phy_modify_paged(0x0ac1, 0x12, 0x0080, 0)?;
        self.phy_modify_paged(0x0ac8, 0x10, 0x0300, 0)?;
        self.phy_modify_paged(0x0ac5, 0x17, 0x0007, 0x0002)?;
        self.phy_write_paged(0x0ad4, 0x16, 0x00a8)?;
        self.phy_write_paged(0x0ac5, 0x16, 0x01ff)?;
        self.phy_modify_paged(0x0ac8, 0x15, 0x00f0, 0x0030)?;

        self.phy_write(0x1f, 0x0b87)?;
        self.phy_write(0x16, 0x80a2)?;
        self.phy_write(0x17, 0x0153)?;
        self.phy_write(0x16, 0x809c)?;
        self.phy_write(0x17, 0x0153)?;
        self.phy_write(0x1f, 0)?;

        self.phy_param(0x8257, 0xffff, 0x020f)?;
        self.phy_param(0x80ea, 0xffff, 0x7843)?;
        self.phy_modify_paged(0x0d06, 0x14, 0, 0x2000)?;
        self.phy_param(0x81a2, 0, 0x0100)?;
        self.phy_modify_paged(0x0b54, 0x16, 0xff00, 0xdb00)?;
        self.phy_modify_paged(0x0a45, 0x12, 0x0001, 0)?;
        self.phy_modify_paged(0x0a5d, 0x12, 0, 0x0020)?;
        self.phy_modify_paged(0x0ad4, 0x17, 0x0010, 0)?;
        self.phy_modify_paged(0x0a86, 0x15, 0x0001, 0)?;
        self.phy_modify_paged(0x0a44, 0x11, 0, 1 << 11)?;
        self.config_eee_phy_8125a()
    }

    fn hw_phy_config_8125b(&mut self) -> Result<()> {
        self.phy_modify_paged(0x0a44, 0x11, 0, 0x0800)?;
        self.phy_modify_paged(0x0ac4, 0x13, 0x00f0, 0x0090)?;
        self.phy_modify_paged(0x0ad3, 0x10, 0x0003, 0x0001)?;

        self.phy_write(0x1f, 0x0b87)?;
        self.phy_write(0x16, 0x80f5)?;
        self.phy_write(0x17, 0x760e)?;
        self.phy_write(0x16, 0x8107)?;
        self.phy_write(0x17, 0x360e)?;
        self.phy_write(0x16, 0x8551)?;
        self.phy_modify(0x17, 0xff00, 0x0800)?;
        self.phy_write(0x1f, 0)?;

        self.phy_modify_paged(0x0bf0, 0x10, 0xe000, 0xa000)?;
        self.phy_modify_paged(0x0bf4, 0x13, 0x0f00, 0x0300)?;
        for param in [
            0x8044, 0x804a, 0x8050, 0x8056, 0x805c, 0x8062, 0x8068, 0x806e, 0x8074, 0x807a,
        ] {
            self.phy_param(param, 0xffff, 0x2417)?;
        }
        self.phy_modify_paged(0x0a4c, 0x15, 0, 0x0040)?;
        self.phy_modify_paged(0x0bf8, 0x12, 0xe000, 0xa000)?;
        self.phy_modify_paged(0x0a5b, 0x12, 1 << 15, 0)?;
        self.config_eee_phy_8125b()
    }

    fn wait_link_list_ready(&self) {
        for _ in 0..4_200 {
            if self.regs.link_list_ready() {
                return;
            }
            core::hint::spin_loop();
        }
        warn!("RTL8125: timed out waiting for link-list FIFO ready");
    }
}

fn configure_default_copper_autoneg(phy: &mut impl PhyRegisterAccess) -> Result<()> {
    // Standard MII registers are available from the default PHY page.
    phy.write_phy(0x1f, 0)?;
    let ctrl1000 = phy.read_phy(MII_CTRL1000)?;
    phy.write_phy(
        MII_CTRL1000,
        (ctrl1000 & !ADVERTISE_1000_MASK) | ADVERTISE_1000FULL,
    )?;
    // Restart negotiation only after the gigabit mode is advertised.
    let bmcr = phy.read_phy(MII_BMCR)?;
    phy.write_phy(
        MII_BMCR,
        (bmcr & !BMCR_AUTONEG_MASK) | BMCR_ANENABLE | BMCR_ANRESTART,
    )
}

impl PhyRegisterAccess for Rtl8125 {
    fn read_phy(&mut self, reg: u32) -> Result<u16> {
        self.phy_read(reg)
    }

    fn write_phy(&mut self, reg: u32, value: u16) -> Result<()> {
        self.phy_write(reg, value)
    }
}

fn validate_ocp_reg(reg: u32) -> Result<()> {
    if reg & 0xffff_0001 == 0 {
        Ok(())
    } else {
        Err(Error::InvalidOcpAddress { reg })
    }
}

fn spin_delay(iterations: usize) {
    for _ in 0..iterations {
        core::hint::spin_loop();
    }
}

#[derive(Clone, Copy)]
struct EphyInfo {
    offset: u8,
    mask: u16,
    bits: u16,
}

const RTL8125A_EPHY: [EphyInfo; 12] = [
    EphyInfo {
        offset: 0x04,
        mask: 0xffff,
        bits: 0xd000,
    },
    EphyInfo {
        offset: 0x0a,
        mask: 0xffff,
        bits: 0x8653,
    },
    EphyInfo {
        offset: 0x23,
        mask: 0xffff,
        bits: 0xab66,
    },
    EphyInfo {
        offset: 0x20,
        mask: 0xffff,
        bits: 0x9455,
    },
    EphyInfo {
        offset: 0x21,
        mask: 0xffff,
        bits: 0x99ff,
    },
    EphyInfo {
        offset: 0x29,
        mask: 0xffff,
        bits: 0xfe04,
    },
    EphyInfo {
        offset: 0x44,
        mask: 0xffff,
        bits: 0xd000,
    },
    EphyInfo {
        offset: 0x4a,
        mask: 0xffff,
        bits: 0x8653,
    },
    EphyInfo {
        offset: 0x63,
        mask: 0xffff,
        bits: 0xab66,
    },
    EphyInfo {
        offset: 0x60,
        mask: 0xffff,
        bits: 0x9455,
    },
    EphyInfo {
        offset: 0x61,
        mask: 0xffff,
        bits: 0x99ff,
    },
    EphyInfo {
        offset: 0x69,
        mask: 0xffff,
        bits: 0xfe04,
    },
];

const RTL8125B_EPHY: [EphyInfo; 6] = [
    EphyInfo {
        offset: 0x0b,
        mask: 0xffff,
        bits: 0xa908,
    },
    EphyInfo {
        offset: 0x1e,
        mask: 0xffff,
        bits: 0x20eb,
    },
    EphyInfo {
        offset: 0x4b,
        mask: 0xffff,
        bits: 0xa908,
    },
    EphyInfo {
        offset: 0x5e,
        mask: 0xffff,
        bits: 0x20eb,
    },
    EphyInfo {
        offset: 0x22,
        mask: 0x0030,
        bits: 0x0020,
    },
    EphyInfo {
        offset: 0x62,
        mask: 0x0030,
        bits: 0x0020,
    },
];

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum OperationKind {
        Read,
        Write,
    }

    struct RecordingPhy {
        page: u16,
        operations: Vec<(OperationKind, u16, u32, u16)>,
    }

    impl PhyRegisterAccess for RecordingPhy {
        fn read_phy(&mut self, reg: u32) -> Result<u16> {
            // Seed target and unrelated bits to verify clear/set behavior and preservation.
            let value = match (self.page, reg) {
                (0, 0x09) => 0x0500,
                (0, 0x00) => 0x0140,
                _ => 0,
            };
            self.operations
                .push((OperationKind::Read, self.page, reg, value));
            Ok(value)
        }

        fn write_phy(&mut self, reg: u32, value: u16) -> Result<()> {
            self.operations
                .push((OperationKind::Write, self.page, reg, value));
            if reg == 0x1f {
                self.page = value;
            }
            Ok(())
        }
    }

    #[test]
    fn default_copper_autoneg_advertises_gigabit_before_restarting() {
        let mut phy = RecordingPhy {
            page: 0x0b87,
            operations: Vec::new(),
        };

        configure_default_copper_autoneg(&mut phy).unwrap();

        assert_eq!(
            phy.operations,
            [
                (OperationKind::Write, 0x0b87, 0x1f, 0),
                (OperationKind::Read, 0, 0x09, 0x0500),
                (OperationKind::Write, 0, 0x09, 0x0600),
                (OperationKind::Read, 0, 0x00, 0x0140),
                (OperationKind::Write, 0, 0x00, 0x1340),
            ]
        );
    }
}
