//! 芯片特定配置函数
//!
//! 从芯片读取版本信息并验证。

use sdio_host::{SdioHost, error::SdioError};

use super::{
    super::protocol::ipc_mem_read, CHIP_ID_H_MASK, CHIP_ID_H_VALUE, CHIP_REV_ADDR,
    CHIP_REV_HIGH_SHIFT, CHIP_REV_MASK, CHIP_REV_U01, CHIP_REV_U02, CHIP_REV_U03, CHIP_REV_U04,
    ChipRevision, ChipVariant,
};

/// 从芯片读取版本信息
pub fn read_chip_revision<H: SdioHost>(
    transport: &mut super::super::protocol::IpcTransport<H>,
    chip: ChipVariant,
) -> Result<ChipRevision, SdioError> {
    let raw = ipc_mem_read(transport, CHIP_REV_ADDR)?;

    let (rev, is_chip_id_h) = match chip {
        ChipVariant::Aic8801 => {
            // AIC8801: 直接取高 16 位的低 8 位
            let rev = (raw >> CHIP_REV_HIGH_SHIFT) as u8;
            (rev, false)
        }
        ChipVariant::Aic8800DC | ChipVariant::Aic8800DW | ChipVariant::Aic8800D80 => {
            // AIC8800DC: 低 6 位为版本号, 高 2 位为 chip_id_h 标志
            let high_part = (raw >> CHIP_REV_HIGH_SHIFT) & CHIP_REV_MASK;
            let rev = high_part as u8;
            let is_h = ((raw >> CHIP_REV_HIGH_SHIFT) & CHIP_ID_H_MASK) == CHIP_ID_H_VALUE;
            (rev, is_h)
        }
        ChipVariant::Aic8800D80X2 => {
            let high_part = (raw >> CHIP_REV_HIGH_SHIFT) & CHIP_REV_MASK;
            let rev = high_part as u8;
            (rev, false)
        }
        ChipVariant::Unknown => return Err(SdioError::Unsupported),
    };
    log::debug!("[aic8800] chip_rev={}, is_chip_id_h={}", rev, is_chip_id_h);
    Ok(ChipRevision { rev, is_chip_id_h })
}

/// 验证芯片版本是否受支持
pub fn validate_chip_revision(chip: ChipVariant, rev: &ChipRevision) -> Result<(), SdioError> {
    let supported = match chip {
        ChipVariant::Aic8801 => {
            // AIC8801: 支持 U02(3), U03(7), U04(7)
            // 由于 U03 == U04 == 7, 只需检查 U02 和 U03
            rev.rev == CHIP_REV_U02 || rev.rev == CHIP_REV_U03 || rev.rev == CHIP_REV_U04
        }
        ChipVariant::Aic8800DC | ChipVariant::Aic8800DW | ChipVariant::Aic8800D80 => {
            // AIC8800DC/DW/D80: 支持 U01(1), U02(3), U03(7), U04(7)
            rev.rev == CHIP_REV_U01 || rev.rev == CHIP_REV_U02 || rev.rev == CHIP_REV_U03 // CHIP_REV_U04 == CHIP_REV_U03 == 7, 已隐式覆盖
        }
        ChipVariant::Aic8800D80X2 => {
            // AIC8800D80X2: 需要 >= CHIP_REV_U04 + 8 = 15
            rev.rev >= CHIP_REV_U04 + 8
        }
        ChipVariant::Unknown => false,
    };
    if !supported {
        log::error!(
            "[aic8800] Unsupported chip revision: chip={:?}, rev={}",
            chip,
            rev.rev
        );
        return Err(SdioError::Unsupported);
    }

    log::debug!(
        "[aic8800] Chip revision validated: {:?}, rev={}",
        chip,
        rev.rev
    );
    Ok(())
}
