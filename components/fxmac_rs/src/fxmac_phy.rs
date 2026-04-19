//! PHY management for FXMAC Ethernet controller.
//!
//! This module provides functions for PHY initialization, configuration,
//! and management through the MDIO interface.

use crate::{fxmac::*, fxmac_const::*};

/// PHY Control Register offset (MII register 0).
pub const PHY_CONTROL_REG_OFFSET: u32 = 0;
pub const PHY_STATUS_REG_OFFSET: u32 = 1;
pub const PHY_IDENTIFIER_1_REG: u32 = 2;
pub const PHY_IDENTIFIER_2_REG: u32 = 3;
pub const PHY_AUTONEGO_ADVERTISE_REG: u32 = 4;
pub const PHY_PARTNER_ABILITIES_1_REG_OFFSET: u32 = 5;
pub const PHY_PARTNER_ABILITIES_2_REG_OFFSET: u32 = 8;
pub const PHY_PARTNER_ABILITIES_3_REG_OFFSET: u32 = 10;
pub const PHY_1000_ADVERTISE_REG_OFFSET: u32 = 9;
pub const PHY_MMD_ACCESS_CONTROL_REG: u32 = 13;
pub const PHY_MMD_ACCESS_ADDRESS_DATA_REG: u32 = 14;
pub const PHY_SPECIFIC_STATUS_REG: u32 = 17;
pub const PHY_CONTROL_FULL_DUPLEX_MASK: u16 = 0x0100;
pub const PHY_CONTROL_LINKSPEED_MASK: u16 = 0x0040;
pub const PHY_CONTROL_LINKSPEED_1000M: u16 = 0x0040;
pub const PHY_CONTROL_LINKSPEED_100M: u16 = 0x2000;
pub const PHY_CONTROL_LINKSPEED_10M: u16 = 0x0000;
pub const PHY_CONTROL_RESET_MASK: u16 = 0x8000;
pub const PHY_CONTROL_LOOPBACK_MASK: u16 = 0x4000;
pub const PHY_CONTROL_AUTONEGOTIATE_ENABLE: u16 = 0x1000;
pub const PHY_CONTROL_AUTONEGOTIATE_RESTART: u16 = 0x0200;
pub const PHY_STATUS_AUTONEGOTIATE_COMPLETE: u16 = 0x0020;
pub const PHY_STAT_LINK_STATUS: u16 = 0x0004;
pub const PHY_AUTOADVERTISE_ASYMMETRIC_PAUSE_MASK: u16 = 0x0800;
pub const PHY_AUTOADVERTISE_PAUSE_MASK: u16 = 0x0400;
pub const PHY_AUTOADVERTISE_AUTONEG_ERROR_MASK: u16 = 0x8000;

// Advertisement control register.
pub const PHY_AUTOADVERTISE_10HALF: u16 = 0x0020; /* Try for 10mbps half-duplex  */
pub const PHY_AUTOADVERTISE_1000XFULL: u16 = 0x0020; /* Try for 1000BASE-X full-duplex */
pub const PHY_AUTOADVERTISE_10FULL: u16 = 0x0040; /* Try for 10mbps full-duplex  */
pub const PHY_AUTOADVERTISE_1000XHALF: u16 = 0x0040; /* Try for 1000BASE-X half-duplex */
pub const PHY_AUTOADVERTISE_100HALF: u16 = 0x0080; /* Try for 100mbps half-duplex */
pub const PHY_AUTOADVERTISE_1000XPAUSE: u16 = 0x0080; /* Try for 1000BASE-X pause    */
pub const PHY_AUTOADVERTISE_100FULL: u16 = 0x0100; /* Try for 100mbps full-duplex */
pub const PHY_AUTOADVERTISE_1000XPSE_ASYM: u16 = 0x0100; /* Try for 1000BASE-X asym pause */
pub const PHY_AUTOADVERTISE_100BASE4: u16 = 0x0200; /* Try for 100mbps 4k packets  */
pub const PHY_AUTOADVERTISE_100_AND_10: u16 = (PHY_AUTOADVERTISE_10FULL
    | PHY_AUTOADVERTISE_100FULL
    | PHY_AUTOADVERTISE_10HALF
    | PHY_AUTOADVERTISE_100HALF);
pub const PHY_AUTOADVERTISE_100: u16 = (PHY_AUTOADVERTISE_100FULL | PHY_AUTOADVERTISE_100HALF);
pub const PHY_AUTOADVERTISE_10: u16 = (PHY_AUTOADVERTISE_10FULL | PHY_AUTOADVERTISE_10HALF);
pub const PHY_AUTOADVERTISE_1000: u16 = 0x0300;
pub const PHY_SPECIFIC_STATUS_SPEED_1000M: u16 = (2 << 14);
pub const PHY_SPECIFIC_STATUS_SPEED_100M: u16 = (1 << 14);
pub const PHY_SPECIFIC_STATUS_SPEED_0M: u16 = (0 << 14);

pub const FT_SUCCESS: u32 = 0;
pub const XMAC_PHY_RESET_ENABLE: u32 = 1;
pub const XMAC_PHY_RESET_DISABLE: u32 = 0;
pub const FXMAC_PHY_AUTONEGOTIATION_DISABLE: u32 = 0;
pub const FXMAC_PHY_AUTONEGOTIATION_ENABLE: u32 = 1;
pub const FXMAC_PHY_MODE_HALFDUPLEX: u32 = 0;
pub const FXMAC_PHY_MODE_FULLDUPLEX: u32 = 1;
pub const FXMAC_PHY_MAX_NUM: u32 = 32;

/// Writes data to a PHY register via MDIO.
///
/// Writes a 16-bit value to the specified register of the PHY at the given
/// address. The MAC provides MDIO access to PHYs adhering to the IEEE 802.3
/// Media Independent Interface (MII) standard.
///
/// # Arguments
///
/// * `instance_p` - Mutable reference to the FXMAC instance.
/// * `phy_address` - PHY address on the MDIO bus (0-31).
/// * `register_num` - PHY register number to write.
/// * `phy_data` - 16-bit data to write to the register.
///
/// # Returns
///
/// * `0` (FT_SUCCESS) on successful write.
/// * `6` (FXMAC_ERR_PHY_BUSY) if the MDIO bus is busy.
///
/// # Note
///
/// The device does not need to be stopped before PHY access, but the MDIO
/// clock should be properly configured.
pub fn FXmacPhyWrite(
    instance_p: &mut FXmac,
    phy_address: u32,
    register_num: u32,
    phy_data: u16,
) -> u32 {
    let mut mgtcr: u32 = 0;
    let mut ipisr: u32 = 0;
    let mut ip_write_temp: u32 = 0;
    let mut status: u32 = 0;

    debug!(
        "FXmacPhyWrite, phy_address={:#x}, register_num={}, phy_data={:#x}",
        phy_address, register_num, phy_data
    );

    // Make sure no other PHY operation is currently in progress
    if (read_reg((instance_p.config.base_address + FXMAC_NWSR_OFFSET) as *const u32)
        & FXMAC_NWSR_MDIOIDLE_MASK)
        == 0
    {
        status = 6; // FXMAC_ERR_PHY_BUSY;
        error!("FXmacPhyRead error: PHY busy!");
    } else {
        // Construct mgtcr mask for the operation
        mgtcr = FXMAC_PHYMNTNC_OP_MASK
            | FXMAC_PHYMNTNC_OP_W_MASK
            | (phy_address << FXMAC_PHYMNTNC_PHAD_SHFT_MSK)
            | (register_num << FXMAC_PHYMNTNC_PREG_SHFT_MSK)
            | phy_data as u32;

        // Write mgtcr and wait for completion
        write_reg(
            (instance_p.config.base_address + FXMAC_PHYMNTNC_OFFSET) as *mut u32,
            mgtcr,
        );

        loop {
            ipisr = read_reg((instance_p.config.base_address + FXMAC_NWSR_OFFSET) as *const u32);
            ip_write_temp = ipisr;

            if (ip_write_temp & FXMAC_NWSR_MDIOIDLE_MASK) != 0 {
                break;
            }
        }

        status = 0; // FT_SUCCESS;
    }

    status
}

/// Reads data from a PHY register via MDIO.
///
/// Reads a 16-bit value from the specified register of the PHY at the given
/// address.
///
/// # Arguments
///
/// * `instance_p` - Mutable reference to the FXMAC instance.
/// * `phy_address` - PHY address on the MDIO bus (0-31).
/// * `register_num` - PHY register number to read.
/// * `phydat_aptr` - Mutable reference to store the read data.
///
/// # Returns
///
/// * `0` (FT_SUCCESS) on successful read.
/// * `6` (FXMAC_ERR_PHY_BUSY) if the MDIO bus is busy.
pub fn FXmacPhyRead(
    instance_p: &mut FXmac,
    phy_address: u32,
    register_num: u32,
    phydat_aptr: &mut u16,
) -> u32 {
    let mut mgtcr: u32 = 0;
    let mut ipisr: u32 = 0;
    let mut IpReadTemp: u32 = 0;
    let mut status: u32 = 0;

    // Make sure no other PHY operation is currently in progress
    if (read_reg((instance_p.config.base_address + FXMAC_NWSR_OFFSET) as *const u32)
        & FXMAC_NWSR_MDIOIDLE_MASK)
        == 0
    {
        status = 6;
        error!("FXmacPhyRead error: PHY busy!");
    } else {
        // Construct mgtcr mask for the operation
        mgtcr = FXMAC_PHYMNTNC_OP_MASK
            | FXMAC_PHYMNTNC_OP_R_MASK
            | (phy_address << FXMAC_PHYMNTNC_PHAD_SHFT_MSK)
            | (register_num << FXMAC_PHYMNTNC_PREG_SHFT_MSK);

        // Write mgtcr and wait for completion
        write_reg(
            (instance_p.config.base_address + FXMAC_PHYMNTNC_OFFSET) as *mut u32,
            mgtcr,
        );

        loop {
            ipisr = read_reg((instance_p.config.base_address + FXMAC_NWSR_OFFSET) as *const u32);
            IpReadTemp = ipisr;

            if (IpReadTemp & FXMAC_NWSR_MDIOIDLE_MASK) != 0 {
                break;
            }
        }

        // Read data
        *phydat_aptr =
            read_reg((instance_p.config.base_address + FXMAC_PHYMNTNC_OFFSET) as *const u32) as u16;

        debug!(
            "FXmacPhyRead, phy_address={:#x}, register_num={}, phydat_aptr={:#x}",
            phy_address, register_num, phydat_aptr
        );

        status = 0;
    }

    status
}

/// Initializes the PHY for the FXMAC controller.
///
/// This function performs PHY detection, optional reset, and speed/duplex
/// configuration. It supports both auto-negotiation and manual speed setting.
///
/// # Arguments
///
/// * `instance_p` - Mutable reference to the FXMAC instance.
/// * `reset_flag` - Set to `XMAC_PHY_RESET_ENABLE` to perform a PHY reset
///                  before configuration, or `XMAC_PHY_RESET_DISABLE` to skip.
///
/// # Returns
///
/// * `0` (FT_SUCCESS) on successful initialization.
/// * `7` (FXMAC_PHY_IS_NOT_FOUND) if no PHY is detected.
/// * `8` (FXMAC_PHY_AUTO_AUTONEGOTIATION_FAILED) if auto-negotiation fails.
/// * Other error codes for PHY communication failures.
///
/// # Example
///
/// ```ignore
/// // Initialize PHY with reset
/// let result = FXmacPhyInit(fxmac, XMAC_PHY_RESET_ENABLE);
/// if result == 0 {
///     println!("PHY initialized, speed: {} Mbps", fxmac.config.speed);
/// }
/// ```
pub fn FXmacPhyInit(instance_p: &mut FXmac, reset_flag: u32) -> u32 {
    let speed = instance_p.config.speed;
    let duplex_mode = instance_p.config.duplex;
    let autonegotiation_en = instance_p.config.auto_neg;
    info!(
        "FXmacPhyInit, speed={}, duplex_mode={}, autonegotiation_en={}, reset_flag={}",
        speed, duplex_mode, autonegotiation_en, reset_flag
    );
    let mut ret: u32 = 0;
    let mut phy_addr: u32 = 0;
    if FXmacDetect(instance_p, &mut phy_addr) != 0 {
        error!("Phy is not found.");
        return 7; //FXMAC_PHY_IS_NOT_FOUND;
    }
    info!("Setting phy addr is {}", phy_addr);
    instance_p.phy_address = phy_addr;
    if reset_flag != 0 {
        FXmacPhyReset(instance_p, phy_addr);
    }
    if autonegotiation_en != 0 {
        ret = FXmacGetIeeePhySpeed(instance_p, phy_addr);
        if ret != 0 {
            return ret;
        }
    } else {
        info!("Set the communication speed manually.");
        assert!(speed != FXMAC_SPEED_1000, "The speed must be 100M or 10M!");

        ret = FXmacConfigureIeeePhySpeed(instance_p, phy_addr, speed, duplex_mode);
        if ret != 0 {
            error!("Failed to manually set the phy.");
            return ret;
        }
    }
    instance_p.link_status = FXMAC_LINKUP;

    0 //FT_SUCCESS
}

pub fn FXmacDetect(instance_p: &mut FXmac, phy_addr_p: &mut u32) -> u32 {
    let mut phy_addr: u32 = 0;
    let mut phy_reg: u16 = 0;
    let mut phy_id1_reg: u16 = 0;
    let mut phy_id2_reg: u16 = 0;

    for phy_addr in 0..FXMAC_PHY_MAX_NUM {
        let mut ret: u32 = FXmacPhyRead(instance_p, phy_addr, PHY_STATUS_REG_OFFSET, &mut phy_reg);
        if (ret != FT_SUCCESS) {
            error!("Phy operation is busy.");
            return ret;
        }
        info!("Phy status reg is {:#x}", phy_reg);

        if (phy_reg != 0xffff) {
            ret = FXmacPhyRead(instance_p, phy_addr, PHY_IDENTIFIER_1_REG, &mut phy_id1_reg);
            ret |= FXmacPhyRead(instance_p, phy_addr, PHY_IDENTIFIER_2_REG, &mut phy_id2_reg);
            info!(
                "Phy id1 reg is {:#x}, Phy id2 reg is {:#x}",
                phy_id1_reg, phy_id2_reg
            );

            if ((ret == FT_SUCCESS)
                && (phy_id2_reg != 0)
                && (phy_id2_reg != 0xffff)
                && (phy_id1_reg != 0xffff))
            {
                *phy_addr_p = phy_addr;
                // phy_addr_b = phy_addr;
                info!("Phy addr is {:#x}", phy_addr);
                return FT_SUCCESS;
            }
        }
    }

    7 //FXMAC_PHY_IS_NOT_FOUND
}

/// FXmacPhyReset: Perform phy software reset
pub fn FXmacPhyReset(instance_p: &mut FXmac, phy_addr: u32) -> u32 {
    let mut control: u16 = 0;

    let mut ret: u32 = FXmacPhyRead(instance_p, phy_addr, PHY_CONTROL_REG_OFFSET, &mut control);
    if (ret != FT_SUCCESS) {
        error!("FXmacPhyReset, read PHY_CONTROL_REG_OFFSET is error");
        return ret;
    }

    control |= PHY_CONTROL_RESET_MASK;

    ret = FXmacPhyWrite(instance_p, phy_addr, PHY_CONTROL_REG_OFFSET, control);
    if (ret != FT_SUCCESS) {
        error!("FXmacPhyReset, write PHY_CONTROL_REG_OFFSET is error");
        return ret;
    }

    loop {
        ret = FXmacPhyRead(instance_p, phy_addr, PHY_CONTROL_REG_OFFSET, &mut control);
        if (ret != FT_SUCCESS) {
            error!("FXmacPhyReset, read PHY_CONTROL_REG_OFFSET is error");
            return ret;
        }
        if (control & PHY_CONTROL_RESET_MASK) == 0 {
            break;
        }
    }

    info!("Phy reset end.");
    ret
}

pub fn FXmacGetIeeePhySpeed(instance_p: &mut FXmac, phy_addr: u32) -> u32 {
    let mut temp: u16 = 0;
    let mut temp2: u16 = 0;
    let mut control: u16 = 0;
    let mut status: u16 = 0;
    let mut negotitation_timeout_cnt: u32 = 0;

    info!("Start phy auto negotiation.");

    let mut ret: u32 = FXmacPhyRead(instance_p, phy_addr, PHY_CONTROL_REG_OFFSET, &mut control);
    if (ret != FT_SUCCESS) {
        error!("FXmacGetIeeePhySpeed,read PHY_CONTROL_REG_OFFSET is error");
        return ret;
    }

    control |= PHY_CONTROL_AUTONEGOTIATE_ENABLE;
    control |= PHY_CONTROL_AUTONEGOTIATE_RESTART;
    ret = FXmacPhyWrite(instance_p, phy_addr, PHY_CONTROL_REG_OFFSET, control);
    if (ret != FT_SUCCESS) {
        error!("FXmacGetIeeePhySpeed,write PHY_CONTROL_REG_OFFSET is error");
        return ret;
    }

    info!("Waiting for phy to complete auto negotiation.");
    loop {
        // 睡眠50毫秒
        crate::utils::msdelay(50);

        ret = FXmacPhyRead(instance_p, phy_addr, PHY_STATUS_REG_OFFSET, &mut status);
        if (ret != FT_SUCCESS) {
            error!("FXmacGetIeeePhySpeed,read PHY_STATUS_REG_OFFSET is error");
            return ret;
        }

        negotitation_timeout_cnt += 1;
        if (negotitation_timeout_cnt >= 0xff) {
            error!("Auto negotiation is error.");
            return 8; //FXMAC_PHY_AUTO_AUTONEGOTIATION_FAILED;
        }

        if (status & PHY_STATUS_AUTONEGOTIATE_COMPLETE) != 0 {
            break;
        }
    }

    info!("Auto negotiation complete.");

    ret = FXmacPhyRead(instance_p, phy_addr, PHY_SPECIFIC_STATUS_REG, &mut temp);
    if (ret != FT_SUCCESS) {
        error!("FXmacGetIeeePhySpeed,read PHY_SPECIFIC_STATUS_REG is error");
        return ret;
    }

    info!("Temp is {:#x}", temp);
    ret = FXmacPhyRead(instance_p, phy_addr, PHY_STATUS_REG_OFFSET, &mut temp2);
    if (ret != FT_SUCCESS) {
        error!("FXmacGetIeeePhySpeed,read PHY_STATUS_REG_OFFSET is error");
        return ret;
    }

    info!("Temp2 is {:#x}", temp2);

    if (temp & (1 << 13)) != 0 {
        info!("Duplex is full.");
        instance_p.config.duplex = 1;
    } else {
        info!("Duplex is half.");
        instance_p.config.duplex = 0;
    }

    if (temp & 0xC000) == PHY_SPECIFIC_STATUS_SPEED_1000M {
        info!("Speed is 1000M.");
        instance_p.config.speed = 1000;
    } else if (temp & 0xC000) == PHY_SPECIFIC_STATUS_SPEED_100M {
        info!("Speed is 100M.");
        instance_p.config.speed = 100;
    } else {
        info!("Speed is 10M.");
        instance_p.config.speed = 10;
    }

    FT_SUCCESS
}

pub fn FXmacConfigureIeeePhySpeed(
    instance_p: &mut FXmac,
    phy_addr: u32,
    speed: u32,
    duplex_mode: u32,
) -> u32 {
    let mut control: u16 = 0;
    let mut autonereg: u16 = 0;
    let mut specific_reg: u16 = 0;

    info!(
        "Manual setting, phy_addr is {:#x},speed {}, duplex_mode is {}.",
        phy_addr, speed, duplex_mode
    );

    let mut ret: u32 = FXmacPhyRead(
        instance_p,
        phy_addr,
        PHY_AUTONEGO_ADVERTISE_REG,
        &mut autonereg,
    );
    if (ret != FT_SUCCESS) {
        error!("FXmacConfigureIeeePhySpeed, read PHY_AUTONEGO_ADVERTISE_REG is error.");
        return ret;
    }

    autonereg |= PHY_AUTOADVERTISE_ASYMMETRIC_PAUSE_MASK;
    autonereg |= PHY_AUTOADVERTISE_PAUSE_MASK;
    ret = FXmacPhyWrite(instance_p, phy_addr, PHY_AUTONEGO_ADVERTISE_REG, autonereg);
    if (ret != FT_SUCCESS) {
        error!("FXmacConfigureIeeePhySpeed, write PHY_AUTONEGO_ADVERTISE_REG is error.");
        return ret;
    }

    ret = FXmacPhyRead(instance_p, phy_addr, PHY_CONTROL_REG_OFFSET, &mut control);
    if (ret != FT_SUCCESS) {
        error!("FXmacConfigureIeeePhySpeed, read PHY_AUTONEGO_ADVERTISE_REG is error.");
        return ret;
    }
    info!("PHY_CONTROL_REG_OFFSET is {:#x}.", control);

    control &= !PHY_CONTROL_LINKSPEED_1000M;
    control &= !PHY_CONTROL_LINKSPEED_100M;
    control &= !PHY_CONTROL_LINKSPEED_10M;

    if speed == 100 {
        control |= PHY_CONTROL_LINKSPEED_100M;
    } else if speed == 10 {
        control |= PHY_CONTROL_LINKSPEED_10M;
    }

    if duplex_mode == 1 {
        control |= PHY_CONTROL_FULL_DUPLEX_MASK;
    } else {
        control &= !PHY_CONTROL_FULL_DUPLEX_MASK;
    }

    // disable auto-negotiation
    control &= !PHY_CONTROL_AUTONEGOTIATE_ENABLE;
    control &= !PHY_CONTROL_AUTONEGOTIATE_RESTART;

    ret = FXmacPhyWrite(instance_p, phy_addr, PHY_CONTROL_REG_OFFSET, control); /* Technology Ability Field */
    if (ret != FT_SUCCESS) {
        error!("FXmacConfigureIeeePhySpeed, write PHY_AUTONEGO_ADVERTISE_REG is error.");
        return ret;
    }

    // FDriverMdelay(1500);
    crate::utils::msdelay(1500);

    info!("Manual selection completed.");

    ret = FXmacPhyRead(
        instance_p,
        phy_addr,
        PHY_SPECIFIC_STATUS_REG,
        &mut specific_reg,
    );
    if (ret != FT_SUCCESS) {
        error!("FXmacConfigureIeeePhySpeed, read PHY_SPECIFIC_STATUS_REG is error.");
        return ret;
    }

    info!("Specific reg is {:#x}", specific_reg);

    if (specific_reg & (1 << 13)) != 0 {
        info!("Duplex is full.");
        instance_p.config.duplex = 1;
    } else {
        info!("Duplex is half.");
        instance_p.config.duplex = 0;
    }

    if (specific_reg & 0xC000) == PHY_SPECIFIC_STATUS_SPEED_100M {
        info!("Speed is 100M.");
        instance_p.config.speed = 100;
    } else {
        info!("Speed is 10M.");
        instance_p.config.speed = 10;
    }

    FT_SUCCESS
}
