//! AIC8800DC/DW 固件 bring-up 序列
//!
//! 移植自 vendor BSP `aic8800dc_compat.c` 的 `aicwifi_init()` DC 分支：
//!   1. `system_config_8800dc` — 读 chip_id/sub_id、set_bbpll_config、syscfg 表
//!   2. `rwnx_plat_patch_load` — patch 固件上传到 0x180000 + misc_ram_init
//!   3. `aicwf_patch_config_8800dc` — wifisetting + ldpc/agc/txgain + patch_tbl
//!   4. `start_from_bootrom_8800DC` — 从 0x120000 启动 (HOST_START_APP_DUMMY)
//!
//! DC 的 FMAC 固件常驻 ROM，host 只上传 patch 覆盖；与 D80/8801 把整份
//! 固件灌进 RAM 的模型不同。

use sdio_host::{SdioHost, error::SdioError};

use super::{
    super::{
        chip::*,
        protocol::{
            IpcTransport, ipc_mem_block_write, ipc_mem_mask_write, ipc_mem_read, ipc_mem_write,
            ipc_start_app,
        },
    },
    data,
};

/// system_config 读 chip_id 的内存地址
const CHIP_ID_ADDR: u32 = 0x4050_0000;
/// system_config 读 chip_sub_id 的内存地址
const CHIP_SUB_ID_ADDR: u32 = 0x0000_0020;
/// patch_config 的配置指针基址 (cfg_base)
const CFG_BASE: u32 = 0x0001_0164;
/// CHIP_ID_H 掩码 (sub_id 区分普通/H 变体)
const CHIP_ID_H_MASK: u8 = 0xC0;
/// patch_tbl 描述块字节数 (describe_size 124 + 4 字节 base 头)
const PATCH_TBL_DESC_BYTES: usize = 124 + 4;
/// patch_config block 上传分片 (vendor 每次 512 字节)
const CFG_CHUNK: usize = 512;

/// syscfg_tbl_8800dc: 160MHz clk
const SYSCFG_TBL_DC: &[(u32, u32)] = &[(0x4050_0010, 0x0000_0004), (0x4050_0010, 0x0000_0006)];

/// syscfg_tbl_8800dc_sdio_u02 (chip_mcu_id==0 时追加)
const SYSCFG_TBL_DC_SDIO_U02: &[(u32, u32)] = &[
    (0x4003_0000, 0x0003_6DA4), // loop forever after assert_err
    (0x0011_E800, 0xE7FE_4070),
    (0x4003_0084, 0x0011_E800),
    (0x4003_0080, 0x0000_0001),
    (0x4010_001C, 0x0000_0000),
];

/// syscfg_tbl_masked_8800dc: {addr, mask, data} — CONFIG_VRF_DCDC_MODE=y 分支
/// pmic_pmu_init。0x70001000 项在 mcu_id==0 时运行期追加 (1<<8)|(1<<15)。
const SYSCFG_TBL_MASKED_DC: &[(u32, u32, u32)] = &[
    (0x7000_216C, 0x3 << 2, 0x1 << 2),
    (0x7000_21BC, 0x3 << 2, 0x1 << 2),
    (
        0x7000_2118,
        (0x7 << 4) | (0x1 << 7),
        (0x2 << 4) | (0x1 << 7),
    ),
    (0x7000_2104, 0x3F | (0x1 << 6), 0x2 | (0x1 << 6)),
    (0x7000_210C, 0x3F | (0x1 << 6), 0x2 | (0x1 << 6)),
    (0x7000_2170, 0xF, 0x1),
    (0x7000_2190, 0x3F, 24),
    (0x7000_21CC, (0x7 << 4) | (0x1 << 7), 0x0),
    (0x7000_10A0, 0x1 << 11, 0x1 << 11),
    (0x7000_1034, (0x1 << 20) | (0x7 << 26), 0x2 << 26),
    (0x7000_1038, 0x1 << 8, 0x1 << 8),
    (0x7000_1094, 0x3 << 2, 0x0),
    (
        0x7000_21D0,
        (0x1 << 5) | (0x1 << 6),
        (0x1 << 5) | (0x1 << 6),
    ),
    (
        0x7000_1000,
        (0x1 << 0) | (0x1 << 20) | (0x1 << 22),
        (0x1 << 0) | (0x1 << 20),
    ),
    (0x7000_1028, 0xF << 2, 0x1 << 2),
];

/// syscfg_tbl_masked_8800dc_u01 (chip_sub_id==0 追加, low-temp 修正)
const SYSCFG_TBL_MASKED_DC_U01: &[(u32, u32, u32)] = &[
    (0x7000_1000, 0x1 << 16, 0x1 << 16),
    (0x7000_1028, 0x1 << 6, 0x1 << 6),
    (0x7000_1000, 0x1 << 16, 0x0),
];

/// syscfg_tbl_masked_8800dc_h: {addr, mask, data} — IS_CHIP_ID_H() 专属
/// 与非 H 表差异: 0x7000216C 值不同、多 0x70002138/213C/2144、无 0x70001034。
/// 0x70001000 为 CONFIG_VRF_DCDC_MODE=y 分支。
const SYSCFG_TBL_MASKED_DC_H: &[(u32, u32, u32)] = &[
    (
        0x7000_216C,
        (0x3 << 2) | (0x3 << 4),
        (0x2 << 2) | (0x2 << 4),
    ),
    (0x7000_2138, 0xFF, 0xFF),
    (0x7000_213C, 0xFF, 0xFF),
    (0x7000_2144, 0xFF, 0xFF),
    (0x7000_21BC, 0x3 << 2, 0x1 << 2),
    (
        0x7000_2118,
        (0x7 << 4) | (0x1 << 7),
        (0x2 << 4) | (0x1 << 7),
    ),
    (0x7000_2104, 0x3F | (0x1 << 6), 0x2 | (0x1 << 6)),
    (0x7000_210C, 0x3F | (0x1 << 6), 0x2 | (0x1 << 6)),
    (0x7000_2170, 0xF, 0x1),
    (0x7000_2190, 0x3F, 24),
    (0x7000_21CC, (0x7 << 4) | (0x1 << 7), 0x0),
    (0x7000_10A0, 0x1 << 11, 0x1 << 11),
    // 注意: 0x70001034 在 H 表里是注释掉的, 不写
    (0x7000_1038, 0x1 << 8, 0x1 << 8),
    (0x7000_1094, 0x3 << 2, 0x0),
    (
        0x7000_21D0,
        (0x1 << 5) | (0x1 << 6),
        (0x1 << 5) | (0x1 << 6),
    ),
    (
        0x7000_1000,
        (0x1 << 0) | (0x1 << 20) | (0x1 << 22),
        (0x1 << 0) | (0x1 << 20),
    ),
    (0x7000_1028, 0xF << 2, 0x1 << 2),
];

/// 运行期探测到的 DC 芯片标识
struct DcChipId {
    chip_id: u8,
    sub_id: u8,
    mcu_id: u8,
}

impl DcChipId {
    fn is_h(&self) -> bool {
        (self.chip_id & CHIP_ID_H_MASK) == CHIP_ID_H_MASK
    }
}

/// set_bbpll_config: 晶振由 CPU 提供时设置 bbpll
fn set_bbpll_config<H: SdioHost>(transport: &mut IpcTransport<H>) -> Result<(), SdioError> {
    let v = ipc_mem_read(transport, 0x4050_0148)?;
    if v & 0x01 == 0 {
        log::debug!("[aic8800] DC crystal not provided by CPU");
        return Ok(());
    }
    let bb = ipc_mem_read(transport, 0x4050_5010)?;
    if (bb >> 29) == 3 {
        return Ok(()); // already set
    }
    let new = (bb | (0x1 << 29) | (0x1 << 30)) & !(0x1 << 31);
    ipc_mem_write(transport, 0x4050_5010, new)?;
    Ok(())
}

/// system_config_8800dc: 读芯片标识 + bbpll + syscfg 表
fn system_config<H: SdioHost>(transport: &mut IpcTransport<H>) -> Result<DcChipId, SdioError> {
    let md = ipc_mem_read(transport, CHIP_ID_ADDR)?;
    let chip_id = (md >> 16) as u8;
    let mcu_id: u8 = if (md >> 25) & 0x1 == 0 { 1 } else { 0 };
    let sub_id = (ipc_mem_read(transport, CHIP_SUB_ID_ADDR)? & 0xFF) as u8;
    let id = DcChipId {
        chip_id,
        sub_id,
        mcu_id,
    };
    log::info!(
        "[aic8800] DC chip_id=0x{:02x} sub_id=0x{:02x} mcu_id={} is_h={}",
        chip_id,
        sub_id,
        mcu_id,
        id.is_h()
    );

    set_bbpll_config(transport)?;
    let _ = ipc_mem_read(transport, 0x4050_0010)?;

    for &(a, d) in SYSCFG_TBL_DC {
        ipc_mem_write(transport, a, d)?;
    }
    if mcu_id == 0 && (sub_id == 1 || sub_id == 2) {
        for &(a, d) in SYSCFG_TBL_DC_SDIO_U02 {
            ipc_mem_write(transport, a, d)?;
        }
    }

    // masked syscfg: H 变体用专属表 (PMIC/时钟配置不同, 且不写 0x70001034)
    let masked_tbl = if id.is_h() {
        SYSCFG_TBL_MASKED_DC_H
    } else {
        SYSCFG_TBL_MASKED_DC
    };
    for &(a, mask, data) in masked_tbl {
        let (m, dd) = if a == 0x7000_1000 && mcu_id == 0 {
            let extra = (0x1 << 8) | (0x1 << 15);
            (mask | extra, data | extra)
        } else {
            (mask, data)
        };
        ipc_mem_mask_write(transport, a, m, dd)?;
    }
    if sub_id == 0 {
        for &(a, mask, data) in SYSCFG_TBL_MASKED_DC_U01 {
            ipc_mem_mask_write(transport, a, mask, data)?;
        }
    }
    Ok(id)
}

/// misc_ram_init_8800dc: 清零固件指向的 misc ram (12 字节)
fn misc_ram_init<H: SdioHost>(transport: &mut IpcTransport<H>) -> Result<(), SdioError> {
    let misc_ram_addr = ipc_mem_read(transport, CFG_BASE + 0x14)?;
    log::info!("[aic8800] DC misc_ram_addr=0x{:08x}", misc_ram_addr);
    for i in 0..3 {
        ipc_mem_write(transport, misc_ram_addr + i * 4, 0)?;
    }
    Ok(())
}

/// rf_misc_ram_t 中 bit_mask 距结构体起始的偏移 (= 0, bit_mask 是首成员)
const RF_MISC_RAM_BITMASK_OFF: u32 = 0;

/// misc_ram_valid_check_8800dc: 读 misc_ram 的 bit_mask[0..4], 判断校准是否已生效
/// valid 条件: bit_mask[0]==0 && (bit_mask[1]&0xFFF00000)==0x80000000 &&
///             bit_mask[2]==0 && (bit_mask[3]&0xFFFFFF00)==0
fn misc_ram_valid<H: SdioHost>(transport: &mut IpcTransport<H>) -> Result<bool, SdioError> {
    let misc_ram_addr = ipc_mem_read(transport, CFG_BASE + 0x14)?;
    let base = misc_ram_addr + RF_MISC_RAM_BITMASK_OFF;
    let mut bm = [0u32; 4];
    for (i, slot) in bm.iter_mut().enumerate() {
        *slot = ipc_mem_read(transport, base + (i as u32) * 4)?;
    }
    let valid = bm[0] == 0
        && (bm[1] & 0xFFF0_0000) == 0x8000_0000
        && bm[2] == 0
        && (bm[3] & 0xFFFF_FF00) == 0;
    log::info!(
        "[aic8800] DC misc_ram bit_mask={:08x},{:08x},{:08x},{:08x} valid={}",
        bm[0],
        bm[1],
        bm[2],
        bm[3],
        valid
    );
    Ok(valid)
}

/// aicwf_dpd_calib_8800dc (CONFIG_DPD + FORCE_DPD_CALIB 路径)
/// 若 misc_ram 未生效: 上传校准固件 → start_app(0x130009, FNCALL) 跑起来,
/// 由校准固件初始化 RF/misc RAM (含 0x110000 区) 并就地 apply DPD。
/// 校准固件以 FNCALL 方式执行完即返回, 无需读回结果 (FORCE 路径不调用 apply)。
fn dpd_calib<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    calib_fw: &[u8],
) -> Result<(), SdioError> {
    if misc_ram_valid(transport)? {
        log::info!("[aic8800] DC misc ram valid, skip dpd calib");
        return Ok(());
    }
    super::upload::upload_firmware(transport, calib_fw, ROM_FMAC_CALIB_ADDR)?;
    log::info!("[aic8800] DC calib fw uploaded ({} bytes)", calib_fw.len());
    // 入口 0x130009 (Thumb, ROM_FMAC_CALIB_ADDR | 9), FNCALL=4 同步执行
    let status = ipc_start_app(transport, ROM_FMAC_CALIB_ADDR + 9, HOST_START_APP_FNCALL)?;
    log::info!("[aic8800] DC dpd calib done status=0x{:08x}", status);
    Ok(())
}

/// 按 vendor 512 字节分片上传一段 cfg blob 到指定 RAM 地址
fn cfg_block_upload<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    addr: u32,
    blob: &[u8],
) -> Result<(), SdioError> {
    let mut off = 0;
    while off < blob.len() {
        let end = core::cmp::min(off + CFG_CHUNK, blob.len());
        ipc_mem_block_write(transport, addr + off as u32, &blob[off..end])?;
        off = end;
    }
    Ok(())
}

/// aicwf_patch_table_load: 解析 patch_tbl blob 并写入芯片
/// 格式: dst[0]=describe_base; [0..128B] 描述块整体块写; 之后按 (addr,data)
/// 对从 offset 128 开始逐对 mem_write。
fn patch_table_load<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    blob: &[u8],
) -> Result<(), SdioError> {
    if blob.len() < 128 {
        log::error!("[aic8800] DC patch_tbl too small: {}", blob.len());
        return Err(SdioError::Unsupported);
    }
    let describe_base = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
    log::debug!(
        "[aic8800] DC patch_tbl describe_base=0x{:08x}",
        describe_base
    );
    // 描述块: describe_base 处写 124+4 字节 (含头部 base 字)
    ipc_mem_block_write(transport, describe_base, &blob[..PATCH_TBL_DESC_BYTES])?;

    // 从字节偏移 128 起, 每 8 字节一对 (addr, data)
    let mut off = 128;
    while off + 8 <= blob.len() {
        let a = u32::from_le_bytes([blob[off], blob[off + 1], blob[off + 2], blob[off + 3]]);
        let d = u32::from_le_bytes([blob[off + 4], blob[off + 5], blob[off + 6], blob[off + 7]]);
        ipc_mem_write(transport, a, d)?;
        off += 8;
    }
    Ok(())
}

/// aicwf_patch_config_8800dc (testmode==0, sub_id>=1 路径)
/// 读 cfg 指针 → 写 wifisetting → 上传 ldpc/agc/txgain → patch_tbl_load
fn patch_config<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    id: &DcChipId,
) -> Result<(), SdioError> {
    if id.sub_id == 0 {
        // u01 走 jump_tbl/patch_tbl_func 路径, 本移植暂只支持 u02/h_u02
        log::error!("[aic8800] DC sub_id==0 (u01) not supported by this port");
        return Err(SdioError::Unsupported);
    }

    let wifisetting_cfg_addr = ipc_mem_read(transport, CFG_BASE)?;
    let ldpc_cfg_addr = ipc_mem_read(transport, CFG_BASE + 0x8)?;
    let agc_cfg_addr = ipc_mem_read(transport, CFG_BASE + 0xC)?;
    let txgain_cfg_addr = ipc_mem_read(transport, CFG_BASE + 0x10)?;
    log::info!(
        "[aic8800] DC cfg: wifi=0x{:08x} ldpc=0x{:08x} agc=0x{:08x} txgain=0x{:08x}",
        wifisetting_cfg_addr,
        ldpc_cfg_addr,
        agc_cfg_addr,
        txgain_cfg_addr
    );

    // wifisetting (patch_tbl_wifisetting_8800dc_u02, 偏移 0x124):
    // 实测此处已含 0x03001e01, 低3字节(使能/睡眠间隔/pwrctrl)已与 vendor 目标
    // 0x01001E01 一致, 唯一差异是 byte3(03 vs 01)。而写 byte3 会 wedge 我们的
    // SDIO host(改低字节正常、改 byte3 即死)。故保留 byte3, 只设低3字节,
    // 既不卡死、有意义的配置位也正确。
    let ws_addr = wifisetting_cfg_addr + 0x0124;
    let ws_cur = ipc_mem_read(transport, ws_addr)?;
    let ws_val = (0x0100_1E01 & 0x00FF_FFFF) | (ws_cur & 0xFF00_0000);
    log::info!(
        "[aic8800] DC wifisetting [0x{:08x}] cur=0x{:08x} -> 0x{:08x}",
        ws_addr,
        ws_cur,
        ws_val
    );
    ipc_mem_write(transport, ws_addr, ws_val)?;

    cfg_block_upload(transport, ldpc_cfg_addr, data::FW_DC_LDPC_CFG)?;
    cfg_block_upload(transport, agc_cfg_addr, data::FW_DC_AGC_CFG)?;

    // txgain: H 变体用 txgain_map_h, 否则 txgain_map (CONFIG_EXT_FEM_8800DCDW=n)
    let txgain = if id.is_h() {
        data::FW_DC_TXGAIN_MAP_H
    } else {
        data::FW_DC_TXGAIN_MAP
    };
    cfg_block_upload(transport, txgain_cfg_addr, txgain)?;

    // sub_id 1/2 → 从 patch_tbl 文件加载跳转表 (H 变体用专属 tbl)
    let patch_tbl = if id.is_h() {
        data::FW_DC_H_FMAC_PATCH_TBL
    } else {
        data::FW_DC_FMAC_PATCH_TBL
    };
    patch_table_load(transport, patch_tbl)?;
    Ok(())
}

/// AIC8800DC/DW 完整固件 bring-up
pub fn init<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    fw_set: &super::data::FirmwareSet,
) -> Result<(), SdioError> {
    // 1. system_config: 芯片标识 + bbpll + syscfg
    let id = system_config(transport)?;
    log::info!("[aic8800] DC system_config done");

    // 2. patch 固件上传到 ROM_FMAC_PATCH_ADDR (0x180000)
    //    H 变体 (sub_id==2) 用专属 patch, 否则用 fw_set 里的 u02 patch
    let patch_fw = if id.is_h() {
        data::FW_DC_H_PATCH
    } else {
        fw_set.wl_fw
    };
    super::upload::upload_firmware(transport, patch_fw, ROM_FMAC_PATCH_ADDR)?;
    // 校准/初始化: H 变体 (CONFIG_DPD+FORCE_DPD_CALIB) 跑 DPD 校准固件,
    // 由它上电并初始化 RF/misc RAM (0x110000 区); 否则走 misc_ram_init 简化路径。
    if id.is_h() {
        dpd_calib(transport, data::FW_DC_H_CALIB)?;
    } else {
        misc_ram_init(transport)?;
    }
    log::info!("[aic8800] DC patch_load done");

    // 3. patch_config: wifisetting + rf cfg + patch_tbl
    patch_config(transport, &id)?;
    log::info!("[aic8800] DC patch_config done");

    // 4. 从 bootrom 启动: 读 0x120000 后 start_app(0x120000, DUMMY)
    let rd = ipc_mem_read(transport, RAM_FMAC_FW_ADDR)?;
    log::debug!(
        "[aic8800] DC fw mem [0x{:08x}]=0x{:08x}",
        RAM_FMAC_FW_ADDR,
        rd
    );
    let status = ipc_start_app(transport, RAM_FMAC_FW_ADDR, HOST_START_APP_DUMMY)?;
    log::info!("[aic8800] DC start_app status=0x{:08x}", status);
    Ok(())
}
