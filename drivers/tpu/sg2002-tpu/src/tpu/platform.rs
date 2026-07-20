//! TPU 平台操作
//!
//! 对应原 tpu_platform.c 中的核心功能

use super::{
    error::TpuError,
    tdma::{TPUPMU_BUFBASE, TPUPMU_BUFSIZE, TPUPMU_CTRL, TdmaRegs},
    tiu::TiuRegs,
    types::{CmdIdNode, CpuSyncDesc, DmaHeader, TpuPmuEvent},
};

/// TIU 中断回调。
///
/// 参数分别为当前任务序列号和触发中断时观察到的 BD 命令 ID。
pub type TiuIrqCallback = fn(seq_no: u32, bd_cmd_id: u32);

/// Blocking TDMA IRQ wait capability injected by OS glue.
///
/// The portable core has no scheduler. OS glue must sleep until IRQ evidence
/// newer than `observed_generation` exists or until the single watchdog budget
/// expires. Unrelated wakes must not restart that deadline. `true` means new
/// evidence exists; `false` means timeout or teardown. Execution is rejected
/// when no capability was installed.
///
/// SG2002 uses a single pinned maintenance owner. Sleeping here lets camera
/// preprocessing and TPU execution make progress without a polling executor.
pub type WaitIrqFn = fn(observed_generation: u64, timeout_us: u64) -> bool;

/// TPU 寄存器备份信息 (用于挂起/恢复)
#[derive(Debug, Clone, Copy, Default)]
pub struct TpuRegBackup {
    pub tdma_int_mask: u32,
    pub tdma_sync_status: u32,
    pub tiu_ctrl_base_address: u32,

    pub tdma_arraybase0_l: u32,
    pub tdma_arraybase1_l: u32,
    pub tdma_arraybase2_l: u32,
    pub tdma_arraybase3_l: u32,
    pub tdma_arraybase4_l: u32,
    pub tdma_arraybase5_l: u32,
    pub tdma_arraybase6_l: u32,
    pub tdma_arraybase7_l: u32,
    pub tdma_arraybase0_h: u32,
    pub tdma_arraybase1_h: u32,

    pub tdma_des_base: u32,
    pub tdma_dbg_mode: u32,
    pub tdma_dcm_disable: u32,
    pub tdma_ctrl: u32,
}

/// TPU 运行时状态
#[derive(Debug, Clone, Copy, Default)]
pub struct TpuRuntimeState {
    /// 中断已收到标志
    pub irq_received: bool,
    /// 备份的寄存器值
    pub reg_backup: TpuRegBackup,
    /// 当前任务序列号
    pub current_seq_no: u32,
    /// TIU 中断回调
    pub tiu_irq_callback: Option<TiuIrqCallback>,
}

/// 启用 TPU PMU
pub fn pmu_enable(tdma: &TdmaRegs, pmubuf_addr_p: u64, pmubuf_size: u32, event: TpuPmuEvent) {
    // 右移 4 位
    let buf_addr = pmubuf_addr_p >> 4;
    let buf_size = (pmubuf_size as u64) >> 4;

    // 设置 buffer 起始地址和大小
    tdma.write(TPUPMU_BUFBASE, buf_addr as u32);
    tdma.write(TPUPMU_BUFSIZE, buf_size as u32);

    // 设置控制寄存器
    let mut reg_value: u32 = 0;
    reg_value |= 0x1; // enable
    reg_value |= 0x8; // enable_tpu
    reg_value |= 0x10; // enable_tdma
    reg_value |= (event as u32) << 5; // event type
    reg_value |= 0x3 << 8; // burst length = 16
    reg_value |= 0x1 << 10; // ring buffer mode
    reg_value &= !0xFFFF0000; // enable dcm

    tdma.write(TPUPMU_CTRL, reg_value);
}

/// 禁用 TPU PMU
pub fn pmu_disable(tdma: &TdmaRegs) {
    let reg_value = tdma.read(TPUPMU_CTRL);
    tdma.write(TPUPMU_CTRL, reg_value & !0x1);
}

/// 重新同步命令 ID
pub fn resync_cmd_id(tdma: &TdmaRegs, tiu: &TiuRegs) {
    tiu.reset_id();
    tdma.reset_sync_id();
}

/// 处理 TDMA 中断
///
/// 返回是否有错误发生
pub fn handle_tdma_irq(tdma: &TdmaRegs, tiu: &TiuRegs, state: &mut TpuRuntimeState) -> bool {
    let reg_value = tdma.read(super::tdma::TDMA_INT_MASK);
    let int_status = (reg_value >> 16) & !super::tdma::TDMA_MASK_INIT;

    let has_error =
        int_status != super::tdma::TDMA_INT_EOD && int_status != super::tdma::TDMA_INT_EOPMU;

    // 清除中断
    tdma.clear_interrupt();

    // 保存状态
    state.reg_backup.tdma_int_mask = tdma.read(super::tdma::TDMA_INT_MASK);
    state.reg_backup.tdma_sync_status = tdma.read(super::tdma::TDMA_SYNC_STATUS);
    state.reg_backup.tiu_ctrl_base_address = tiu.read_bd_ctrl(0);

    state.irq_received = true;

    has_error
}

/// 轮询等待命令完成
pub fn poll_cmdbuf_done(
    tiu: &TiuRegs,
    id_node: &CmdIdNode,
    state: &mut TpuRuntimeState,
    timeout_checker: impl Fn() -> bool,
) -> Result<(), TpuError> {
    // 检查 TDMA
    if id_node.tdma_cmd_id > 0 {
        let tdma_id = state.reg_backup.tdma_sync_status >> 16;
        if tdma_id < id_node.tdma_cmd_id {
            // return Err(TpuError::TdmaError(tdma_id));
        }
    }

    // 轮询 TIU
    if id_node.bd_cmd_id > 0 {
        loop {
            let reg_val = tiu.read_bd_ctrl(0);
            let current_id = (reg_val >> 6) & 0xFFFF;
            let int_flag = (reg_val & (1 << 1)) != 0;

            if current_id >= id_node.bd_cmd_id && int_flag {
                if let Some(callback) = state.tiu_irq_callback {
                    callback(state.current_seq_no, current_id);
                }
                // 清除中断
                tiu.write_bd_ctrl(0, reg_val | (1 << 1));
                break;
            }

            // 检查超时
            if timeout_checker() {
                return Err(TpuError::Timeout);
            }

            // CPU 放松 (让出执行权给其他任务)
            core::hint::spin_loop();
        }
    }

    Ok(())
}

/// 执行 DMA buffer
///
/// 这是核心执行函数，对应原 platform_run_dmabuf
///
/// # Safety
/// `dmabuf_vaddr` 必须指向一段大小至少为 `DmaHeader` 加上其声明的所有
/// CPU/TDMA/PMU 描述符的有效内存，且地址在调用期间保持有效。
pub unsafe fn run_dmabuf(
    tdma: &TdmaRegs,
    tiu: &TiuRegs,
    dmabuf_vaddr: *const u8,
    dmabuf_paddr: u64,
    state: &mut TpuRuntimeState,
    wait_irq: impl Fn() -> Result<(), TpuError>,
    timeout_checker: impl Fn() -> bool,
) -> Result<(), TpuError> {
    // 解析 header
    let header = unsafe { &*(dmabuf_vaddr as *const DmaHeader) };

    if !header.is_valid() {
        return Err(TpuError::InvalidDmabuf);
    }

    // 检查 DMA buffer 对齐
    if (dmabuf_paddr & 0xFFF) != 0 {
        return Err(TpuError::DmabufNotAligned);
    }

    // 重置状态
    state.irq_received = false;

    // 设置 array base
    tdma.set_array_bases(header);

    // 检查是否启用 PMU
    let pmu_enabled = header.has_valid_pmu();
    let pmubuf_addr = dmabuf_paddr + header.pmubuf_offset as u64;

    if pmu_enabled {
        pmu_enable(
            tdma,
            pmubuf_addr,
            header.pmubuf_size,
            TpuPmuEvent::TdmaBandwidth,
        );
    }

    // 获取 CPU 描述符指针
    let desc_base =
        unsafe { dmabuf_vaddr.add(core::mem::size_of::<DmaHeader>()) as *const CpuSyncDesc };

    // 遍历所有 CPU 描述符
    for i in 0..header.cpu_desc_count {
        let desc = unsafe { &*desc_base.add(i as usize) };

        let bd_num = (desc.num_bd & 0xFFFF) as u32;
        let tdma_num = (desc.num_gdma & 0xFFFF) as u32;
        let bd_offset = desc.offset_bd;
        let tdma_offset = desc.offset_gdma;

        // 重置命令 ID
        resync_cmd_id(tdma, tiu);
        state.irq_received = false;

        let id_node = CmdIdNode {
            bd_cmd_id: bd_num,
            tdma_cmd_id: tdma_num,
        };

        // 启动 TIU
        if bd_num > 0 {
            tiu.fire_descriptor(bd_offset as u64, bd_num);
        }

        // 启动 TDMA
        if tdma_num > 0 {
            tdma.fire_descriptor(tdma_offset as u64, tdma_num);
        }

        // 等待 TDMA 完成
        if tdma_num > 0 {
            wait_irq()?;
        }

        // 检查完成状态
        poll_cmdbuf_done(tiu, &id_node, state, &timeout_checker)?;
    }

    // 禁用 PMU
    if pmu_enabled {
        state.irq_received = false;
        pmu_disable(tdma);
        wait_irq()?;
    }

    Ok(())
}

/// 备份 TPU 寄存器 (挂起时使用)
pub fn backup_registers(tdma: &TdmaRegs, tiu: &TiuRegs, backup: &mut TpuRegBackup) {
    use super::tdma::*;

    backup.tdma_int_mask = tdma.read(TDMA_INT_MASK);
    backup.tdma_sync_status = tdma.read(TDMA_SYNC_STATUS);
    backup.tiu_ctrl_base_address = tiu.read_bd_ctrl(0);

    backup.tdma_arraybase0_l = tdma.read(TDMA_ARRAYBASE0_L);
    backup.tdma_arraybase1_l = tdma.read(TDMA_ARRAYBASE1_L);
    backup.tdma_arraybase2_l = tdma.read(TDMA_ARRAYBASE2_L);
    backup.tdma_arraybase3_l = tdma.read(TDMA_ARRAYBASE3_L);
    backup.tdma_arraybase4_l = tdma.read(TDMA_ARRAYBASE4_L);
    backup.tdma_arraybase5_l = tdma.read(TDMA_ARRAYBASE5_L);
    backup.tdma_arraybase6_l = tdma.read(TDMA_ARRAYBASE6_L);
    backup.tdma_arraybase7_l = tdma.read(TDMA_ARRAYBASE7_L);
    backup.tdma_arraybase0_h = tdma.read(TDMA_ARRAYBASE0_H);
    backup.tdma_arraybase1_h = tdma.read(TDMA_ARRAYBASE1_H);

    backup.tdma_des_base = tdma.read(TDMA_DES_BASE);
    backup.tdma_dbg_mode = tdma.read(TDMA_DEBUG_MODE);
    backup.tdma_dcm_disable = tdma.read(TDMA_DCM_DISABLE);
    backup.tdma_ctrl = tdma.read(TDMA_CTRL);
}

/// 恢复 TPU 寄存器 (恢复时使用)
pub fn restore_registers(tdma: &TdmaRegs, tiu: &TiuRegs, backup: &TpuRegBackup) {
    use super::tdma::*;

    tdma.write(TDMA_INT_MASK, backup.tdma_int_mask);
    tdma.write(TDMA_SYNC_STATUS, backup.tdma_sync_status);
    tiu.write_bd_ctrl(0, backup.tiu_ctrl_base_address);

    tdma.write(TDMA_ARRAYBASE0_L, backup.tdma_arraybase0_l);
    tdma.write(TDMA_ARRAYBASE1_L, backup.tdma_arraybase1_l);
    tdma.write(TDMA_ARRAYBASE2_L, backup.tdma_arraybase2_l);
    tdma.write(TDMA_ARRAYBASE3_L, backup.tdma_arraybase3_l);
    tdma.write(TDMA_ARRAYBASE4_L, backup.tdma_arraybase4_l);
    tdma.write(TDMA_ARRAYBASE5_L, backup.tdma_arraybase5_l);
    tdma.write(TDMA_ARRAYBASE6_L, backup.tdma_arraybase6_l);
    tdma.write(TDMA_ARRAYBASE7_L, backup.tdma_arraybase7_l);
    tdma.write(TDMA_ARRAYBASE0_H, backup.tdma_arraybase0_h);
    tdma.write(TDMA_ARRAYBASE1_H, backup.tdma_arraybase1_h);

    tdma.write(TDMA_DES_BASE, backup.tdma_des_base);
    tdma.write(TDMA_DEBUG_MODE, backup.tdma_dbg_mode);
    tdma.write(TDMA_DCM_DISABLE, backup.tdma_dcm_disable);
}
