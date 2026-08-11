//! 固件上传核心逻辑

use sdio_host::{SdioHost, error::SdioError};

use super::{
    super::{
        chip::*,
        protocol::{
            IpcTransport, ipc_mem_block_write, ipc_mem_mask_write, ipc_mem_read, ipc_mem_write,
            ipc_start_app,
        },
    },
    data::FirmwareSet,
};

const AIC_PATCH_MAGIC_NUM: u32 = 0x4843_5450; // "PTCH"
const AIC_PATCH_MAGIC_NUM_2: u32 = 0x5054_4348; // "HCTP"
const AIC_PATCH_BLOCK_MAX: usize = 4;

const D80_PATCH_TBL: &[[u32; 2]] = &[
    [0x00b4, 0xf3010000], // 2.4GHz only (USE_5G=0)
    [0x0170, 0x0100000a], // AMSDU_RX
    [0x0188, 0x00000001], // user_ext_flags: PWROFST_COVER_CALIB
];

const D80_PATCH_START_ADDR: u32 = 0x0016_F800;
const D80_PATCH_CONFIG_OFFSET: u32 = 0x0198;

/// 将固件二进制数据上传到芯片 RAM
pub fn upload_firmware<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    fw_data: &[u8],
    fw_addr: u32,
) -> Result<(), SdioError> {
    let size = fw_data.len();
    if size == 0 {
        log::warn!("[aic8800] Firmware data is empty, skipping upload");
        return Err(SdioError::Unsupported);
    }

    log::debug!(
        "[aic8800] Uploading firmware: addr=0x{:08x}, size={} bytes",
        fw_addr,
        size
    );

    let mut offset = 0;
    while offset + FW_UPLOAD_CHUNK_SIZE <= size {
        ipc_mem_block_write(
            transport,
            fw_addr.wrapping_add(offset as u32),
            &fw_data[offset..offset + FW_UPLOAD_CHUNK_SIZE],
        )?;
        offset += FW_UPLOAD_CHUNK_SIZE;
    }
    if offset < size {
        ipc_mem_block_write(transport, fw_addr + offset as u32, &fw_data[offset..])?;
    }
    log::info!("[aic8800] Firmware uploaded ({} bytes)", size);
    Ok(())
}

/// AIC8801 Patch 表配置
pub fn aicwifi_patch_config<H: SdioHost>(transport: &mut IpcTransport<H>) -> Result<(), SdioError> {
    let patch_num: u32 = (PATCH_TBL.len() as u32) * 2;
    let start_addr: u32 = PATCH_TBL_START_ADDR;

    let rd_addr = RAM_FMAC_FW_ADDR + FW_CONFIG_BASE_OFFSET;
    let config_base = ipc_mem_read(transport, rd_addr)?;
    log::debug!("[aic8800] config_base = 0x{:08x}", config_base);

    ipc_mem_write(transport, PATCH_ADDR_REG, start_addr)?;
    ipc_mem_write(transport, PATCH_NUM_REG, patch_num)?;

    for (cnt, entry) in PATCH_TBL.iter().enumerate() {
        let offset = (cnt as u32) * 8;
        ipc_mem_write(transport, start_addr + offset, entry[0] + config_base)?;
        ipc_mem_write(transport, start_addr + offset + 4, entry[1])?;
    }

    log::debug!("[aic8800] patch_config done ({} entries)", PATCH_TBL.len());
    Ok(())
}

/// AIC8801 系统配置 (时钟门控 + RF PLL)
pub fn aicwifi_sys_config<H: SdioHost>(transport: &mut IpcTransport<H>) -> Result<(), SdioError> {
    for entry in SYSCFG_TBL_MASKED {
        ipc_mem_mask_write(transport, entry[0], entry[1], entry[2])?;
    }

    for entry in RF_TBL_MASKED {
        ipc_mem_mask_write(transport, entry[0], entry[1], entry[2])?;
    }

    log::debug!("[aic8800] sys_config done");
    Ok(())
}

/// AIC8801 固件初始化流程
pub fn init_aic8801_firmware<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    fw_set: &FirmwareSet,
) -> Result<(), SdioError> {
    upload_firmware(transport, fw_set.wl_fw, RAM_FMAC_FW_ADDR)?;

    if !fw_set.wl_patch.is_empty() {
        upload_firmware(transport, fw_set.wl_patch, RAM_FMAC_FW_PATCH_ADDR)?;
    } else {
        log::warn!("[aic8800] No patch firmware provided, skipping patch upload");
    }

    aicwifi_patch_config(transport)?;
    aicwifi_sys_config(transport)?;

    transport.host().set_clock(FIRMWARE_START_CLOCK_FREQ)?;

    let status = ipc_start_app(transport, RAM_FMAC_FW_ADDR, HOST_START_APP_AUTO)?;
    log::debug!("[aic8800] AIC8801 start_app status = 0x{:08x}", status);

    transport.host().set_clock(DEFAULT_CLOCK_FREQ)?;
    log::info!("[aic8800] AIC8801 firmware init done");
    Ok(())
}

/// AIC8800D80 Patch 配置
pub fn aicwifi_patch_config_8800d80<H: SdioHost>(
    transport: &mut IpcTransport<H>,
) -> Result<(), SdioError> {
    let rd_patch_addr = RAM_FMAC_FW_ADDR + D80_PATCH_CONFIG_OFFSET;
    let aic_patch_addr = rd_patch_addr + 8;
    let mut start_addr: u32 = D80_PATCH_START_ADDR;
    let patch_cnt = D80_PATCH_TBL.len() as u32;

    let config_base = ipc_mem_read(transport, rd_patch_addr)?;
    log::debug!("[aic8800] D80 config_base = 0x{:08x}", config_base);

    let aic_patch_str_base = ipc_mem_read(transport, aic_patch_addr)?;
    log::debug!(
        "[aic8800] D80 aic_patch_str_base = 0x{:08x}",
        aic_patch_str_base
    );

    let rd_version_addr = RAM_FMAC_FW_ADDR + 0x01C;
    let rd_version_val = ipc_mem_read(transport, rd_version_addr)?;
    log::debug!("[aic8800] D80 fw_version = 0x{:08x}", rd_version_val);
    if rd_version_val > 0x0609_0100 {
        let patch_buff_addr = rd_patch_addr + 12;
        let patch_buff_base = ipc_mem_read(transport, patch_buff_addr)?;
        start_addr = patch_buff_base;
    }

    let patch_struct_addr =
        |field_offset: usize| -> u32 { aic_patch_str_base.wrapping_add(field_offset as u32) };

    let magic_num_off = 0;
    let magic_num_2_off = 8;
    let pair_start_off = 4;
    let pair_count_off = 12;
    let block_size_off = 48;

    ipc_mem_write(
        transport,
        patch_struct_addr(magic_num_off),
        AIC_PATCH_MAGIC_NUM,
    )?;
    ipc_mem_write(
        transport,
        patch_struct_addr(magic_num_2_off),
        AIC_PATCH_MAGIC_NUM_2,
    )?;
    ipc_mem_write(transport, patch_struct_addr(pair_start_off), start_addr)?;
    ipc_mem_write(transport, patch_struct_addr(pair_count_off), patch_cnt)?;

    for (cnt, entry) in D80_PATCH_TBL.iter().enumerate() {
        let offset = (cnt as u32) * 8;
        ipc_mem_write(
            transport,
            start_addr + offset,
            entry[0].wrapping_add(config_base),
        )?;
        ipc_mem_write(transport, start_addr + offset + 4, entry[1])?;
    }

    for i in 0..AIC_PATCH_BLOCK_MAX {
        ipc_mem_write(transport, patch_struct_addr(block_size_off + i * 4), 0)?;
    }

    log::debug!(
        "[aic8800] D80 patch_config done ({} entries)",
        D80_PATCH_TBL.len()
    );
    Ok(())
}

/// AIC8800D80 固件初始化流程
pub fn init_aic8800d80_firmware<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    fw_set: &FirmwareSet,
) -> Result<(), SdioError> {
    upload_firmware(transport, fw_set.wl_fw, RAM_FMAC_FW_ADDR)?;
    aicwifi_patch_config_8800d80(transport)?;

    let status = ipc_start_app(transport, RAM_FMAC_FW_ADDR, HOST_START_APP_AUTO)?;
    log::debug!("[aic8800] D80 start_app status = 0x{:08x}", status);

    log::info!("[aic8800] AIC8800D80 firmware init done");
    Ok(())
}
