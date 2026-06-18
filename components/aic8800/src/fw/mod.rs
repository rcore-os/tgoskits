extern crate alloc;

// 模块声明
pub mod chip;
pub mod config;
pub mod firmware;
pub mod protocol;

// 导入必要的类型和函数
// 从 chip 模块重新导出常用类型 (实际来自 aic8800_common)
pub use chip::{ChipRevision, ChipVariant};
use config::*;
pub use firmware::FirmwareSet;
// 从 firmware 模块重新导出固件集合
use firmware::{
    get_firmware_set, init_aic8800d80_firmware, init_aic8800dc_firmware, init_aic8801_firmware,
};
use protocol::{IpcTransport, ipc_mem_write}; // 导入配置常量
use sdio_host::{SdioHost, error::SdioError};

// Re-export aic8800_common 供外部使用
pub use crate::common;
use crate::common::{SDIOWIFI_V3_WAKEUP_VALUE, SDIOWIFI_WAKEUP_REG_V3};
/// SDIO 功能寄存器初始化
///
/// - AIC8801/DC/DW: 非 v3 寄存器组
/// - AIC8800D80/D80X2: v3 寄存器组
///
/// AIC8800DC/DW 额外需要带起 SDIO function 2 (func_msg) 作为命令邮箱:
/// 使能 func2、设块大小 512、写 func2 的 register_block/bytemode/中断使能。
/// 否则 bootrom 命令邮箱不响应,第一笔 DBG_MEM_READ 会一直超时。
pub fn sdio_func_setup<H: SdioHost>(host: &mut H, chip: ChipVariant) -> Result<(), SdioError> {
    let is_v3 = matches!(
        chip,
        ChipVariant::Aic8800D80 | ChipVariant::Aic8800D80X2
    );
    let is_dc = matches!(chip, ChipVariant::Aic8800DC | ChipVariant::Aic8800DW);

    if !is_v3 {
        // ---- AIC8801 / AIC8800DC / AIC8800DW ----

        // AIC8800DC/DW: 命令邮箱在 SDIO function 2, 必须先带起 func2
        if is_dc {
            host.enable_func(2)?;
            host.set_block_size(2, SDIOWIFI_FUNC_BLOCKSIZE)?;
            // func2: 使能块模式 + 禁用字节模式
            host.write_byte(2, SDIOWIFI_REGISTER_BLOCK, 0x01)?;
            host.write_byte(2, SDIOWIFI_BYTEMODE_ENABLE_REG, 0x01)?;
            // func2 中断使能 (bootrom cfm 经由 func2 返回)
            host.write_byte(2, SDIOWIFI_INTR_CONFIG_REG, 0x07)?;
        }

        // 使能块模式 (block_bit0 = 0x1)
        host.write_byte(1, SDIOWIFI_REGISTER_BLOCK, 0x01)?;

        // 禁用字节模式 (byte_mode_disable = 0x1, 即 "no byte mode")
        host.write_byte(1, SDIOWIFI_BYTEMODE_ENABLE_REG, 0x01)?;

        // func1 中断使能
        if is_dc {
            host.write_byte(1, SDIOWIFI_INTR_CONFIG_REG, 0x07)?;
        }

        // 延时等待芯片内部状态稳定 (Linux: mdelay(10))
        crate::runtime::runtime().sleep_ms(10);
    } else {
        // ---- AIC8800D80 / AIC8800D80X2 (SDIO v3) ----

        // Linux aicwf_sdiov3_func_init: write 0x7F to func0 register 0xF2
        host.write_byte(0, 0xF2, 0x7F)?;

        // 禁用字节模式
        host.write_byte(1, SDIOWIFI_BYTEMODE_ENABLE_REG_V3, 0x01)?;

        // 唤醒芯片
        host.write_byte(1, SDIOWIFI_WAKEUP_REG_V3, SDIOWIFI_V3_WAKEUP_VALUE)?;

        // 等待唤醒稳定 (Linux: mdelay(5))
        crate::runtime::runtime().sleep_ms(5);

        // 检查唤醒状态
        let sleep_val = host.read_byte(1, SDIOWIFI_SLEEP_REG_V3)?;
        if sleep_val & SDIOWIFI_V3_SLEEP_READY_BIT == 0 {
            log::error!("[aic8800] V3 wakeup failed, sleep_reg=0x{:02x}", sleep_val);
            return Err(SdioError::Timeout);
        }
        log::info!("[aic8800] V3 SDIO ready (sleep_reg=0x{:02x})", sleep_val);
    }

    log::debug!("[aic8800] SDIO func setup done (chip={:?})", chip);
    Ok(())
}

/// BSP 系统配置 — 在固件上传前调用
///
/// 写入 10 个关键寄存器:
///   - 时钟/PLL 配置 (0x40500014, 0x40500018, 0x40500004)
///   - panic 修复 (0x40040000)
///   - BBPLL 配置 (0x40040084, 0x40040080, 0x40100058)
///   - PMIC 接口初始化 (0x50000000)
///   - 26MHz 晶振分频 (0x50019150)
///   - ★ 停止看门狗 (0x50017008) — 不停止会导致芯片在 ~1s 后复位
fn aicbsp_system_config<H: SdioHost>(ipc: &mut IpcTransport<H>) -> Result<(), SdioError> {
    for &(addr, data) in config::SYSCFG_TBL {
        ipc_mem_write(ipc, addr, data)?;
    }
    log::debug!("[aic8800] aicbsp_system_config done");
    Ok(())
}

/// 完整的固件初始化入口
///
/// fw_data: 固件二进制数据 (fmacfw.bin 或 fw_patch.bin)
pub fn firmware_init<H: SdioHost>(host: &mut H, chip: ChipVariant) -> Result<(), SdioError> {
    log::info!("[aic8800] firmware_init: chip={:?}", chip);

    // 1. SDIO 功能寄存器初始化 (区分芯片型号; DC/DW 会带起 func2)
    let is_v3 = matches!(chip, ChipVariant::Aic8800D80 | ChipVariant::Aic8800D80X2);
    sdio_func_setup(host, chip)?;

    // 2. 时钟配置
    if matches!(chip, ChipVariant::Aic8801) {
        host.set_clock(DEFAULT_CLOCK_FREQ)?;
        log::debug!("[aic8800] SDIO clock set to 25MHz for AIC8801");
    }

    // 3. 创建 IPC 传输层
    let mut ipc = IpcTransport::new(host, chip);

    // 4. 读取芯片版本信息
    let chip_rev = chip::read_chip_revision(&mut ipc, chip)?;
    log::debug!("[aic8800] chip_rev={}", chip_rev.rev);

    // 5. 验证芯片版本是否受支持
    chip::validate_chip_revision(chip, &chip_rev)?;

    // 5.5 BSP 系统配置 (停止看门狗, 配置 PMIC/时钟) — 必须在固件上传前执行
    if matches!(chip, ChipVariant::Aic8801) {
        aicbsp_system_config(&mut ipc)?;
    }

    // 6. 选择固件
    let fw_set = get_firmware_set(chip, &chip_rev).unwrap();
    log::debug!(
        "[aic8800] firmware: {} (fw={} bytes, patch={} bytes)",
        fw_set.desc,
        fw_set.wl_fw.len(),
        fw_set.wl_patch.len(),
    );

    if fw_set.wl_fw.is_empty() {
        log::error!("[aic8800] WiFi firmware data is empty");
        return Err(SdioError::Unsupported);
    }

    // 7. 根据芯片类型执行固件初始化
    match chip {
        ChipVariant::Aic8801 => init_aic8801_firmware(&mut ipc, &fw_set)?,
        ChipVariant::Aic8800DC | ChipVariant::Aic8800DW => {
            init_aic8800dc_firmware(&mut ipc, &fw_set)?
        }
        ChipVariant::Aic8800D80 | ChipVariant::Aic8800D80X2 => {
            init_aic8800d80_firmware(&mut ipc, &fw_set)?
        }
        ChipVariant::Unknown => {
            log::error!("[aic8800] Unknown chip, cannot init firmware");
            return Err(SdioError::Unsupported);
        }
    }

    // 8. D80 固件启动后写入 wakeup_reg=4 (pwrctrl acknowledge)
    if is_v3 {
        host.write_byte(1, SDIOWIFI_WAKEUP_REG_V3, 0x04)?;
        log::debug!("[aic8800] D80 post-init wakeup_reg=0x04 written");
    }

    log::info!("[aic8800] Firmware init complete");

    Ok(())
}
