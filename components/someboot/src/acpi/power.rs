//! ACPI 电源管理实现
//!
//! 根据 ACPI 规范实现基于 FADT 的系统关机功能

use acpi::sdt::fadt::Fadt;
use log::{error, info, warn};

/// ACPI 关机错误
#[derive(Debug)]
pub enum ShutdownError {
    /// ACPI 不可用
    AcpiNotAvailable,
    /// FADT 表不存在
    NoFadt,
    /// PM1 寄存器地址无效
    InvalidPm1Address,
}

impl core::fmt::Display for ShutdownError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ShutdownError::AcpiNotAvailable => write!(f, "ACPI not available"),
            ShutdownError::NoFadt => write!(f, "FADT table not found"),
            ShutdownError::InvalidPm1Address => write!(f, "Invalid PM1 register address"),
        }
    }
}

/// 执行 ACPI 关机
///
/// # 流程
/// 1. 获取 ACPI 表
/// 2. 解析 FADT 表
/// 3. 获取 PM1 控制寄存器地址
/// 4. 提取 S5 睡眠类型值
/// 5. 写入 PM1 控制寄存器触发关机
#[allow(dead_code)]
pub fn shutdown() -> ! {
    info!("Attempting ACPI shutdown...");

    match shutdown_internal() {
        Ok(_) => {
            info!("ACPI shutdown initiated, waiting for power off...");
            // 关机成功,系统应该已经关闭
            // 如果执行到这里,说明关机失败或需要等待
            loop {
                unsafe { core::arch::asm!("idle 0") };
            }
        }
        Err(e) => {
            error!("ACPI shutdown failed: {}", e);
            warn!("Falling back to idle loop");
            // Fallback: 进入无限循环
            loop {
                unsafe { core::arch::asm!("idle 0") };
            }
        }
    }
}

fn shutdown_internal() -> Result<(), ShutdownError> {
    // 1. 获取 ACPI 表
    let tables = crate::acpi::tables().map_err(|_| ShutdownError::AcpiNotAvailable)?;

    // 2. 获取 FADT 表
    let fadt_mapping = tables
        .find_tables::<Fadt>()
        .next()
        .ok_or(ShutdownError::NoFadt)?;

    let fadt = &*fadt_mapping;

    // 3. 获取 PM1 控制寄存器地址
    // 根据 ACPI 规范,PM1a_CNT_BLK 是必须的,PM1b_CNT_BLK 是可选的
    // 使用 FADT 提供的方法来获取 GenericAddress,它会自动处理扩展字段

    let pm1a_legacy = fadt.pm1a_control_block;
    let pm1b_legacy = fadt.pm1b_control_block;
    info!(
        "FADT: pm1a_cnt_blk={:#x}, pm1b_cnt_blk={:#x}",
        pm1a_legacy, pm1b_legacy
    );

    // 使用 FADT 方法获取 PM1 控制寄存器地址
    let pm1a_generic = fadt
        .pm1a_control_block()
        .map_err(|_| ShutdownError::InvalidPm1Address)?;

    let pm1a_cnt = pm1a_generic.address as usize;
    let pm1a_addr = pm1a_generic.address;
    let pm1a_space = format!("{:?}", pm1a_generic.address_space);
    let pm1a_width = pm1a_generic.bit_width;
    info!(
        "PM1a control block: address={:#x}, space={}, bit_width={}",
        pm1a_addr, pm1a_space, pm1a_width
    );

    // pm1b_control_block() 返回 Result<Option<GenericAddress>, AcpiError>
    let pm1b_cnt = match fadt.pm1b_control_block() {
        Ok(Some(generic)) => {
            let addr = generic.address;
            let space = format!("{:?}", generic.address_space);
            info!("PM1b control block: address={:#x}, space={}", addr, space);
            Some(addr as usize)
        }
        _ => None,
    };

    info!("Final: PM1a_CNT={:#x}, PM1b_CNT={:#x?}", pm1a_cnt, pm1b_cnt);

    // 如果 PM1 控制寄存器地址为 0,尝试使用 QEMU 特定的关机方法
    if pm1a_cnt == 0 {
        warn!("PM1 control block address is 0, trying QEMU-specific shutdown");
        return qemu_shutdown();
    }

    // 4. 获取 S5 睡眠类型
    // S5 = Soft Off (关机状态)
    // 根据 ACPI 规范,S5 类型值编码在 FADT 的特定位置
    let (s5_typa, s5_typb) = get_s5_sleep_type(fadt)?;

    info!("S5 sleep type: a={:#x}, b={:#x}", s5_typa, s5_typb);

    // 5. 写入 PM1 控制寄存器
    // SLP_EN 位在 bit 13, 值为 0x2000
    // SLP_TYPa 值在 bit 10-12
    const SLP_EN: u16 = 0x2000;

    let pm1a_value = s5_typa | SLP_EN;
    info!("Writing PM1a_CNT: {:#x}", pm1a_value);

    unsafe {
        // 将物理地址转换为虚拟地址
        let virt_addr = crate::mem::phys_to_virt(pm1a_cnt) as *mut u16;
        // 写入 PM1a 控制寄存器
        core::ptr::write_volatile(virt_addr, pm1a_value);
    }

    // 如果有 PM1b_CNT,也写入
    if let Some(pm1b_addr) = pm1b_cnt {
        let pm1b_value = s5_typb | SLP_EN;
        info!("Writing PM1b_CNT: {:#x}", pm1b_value);

        unsafe {
            let virt_addr = crate::mem::phys_to_virt(pm1b_addr) as *mut u16;
            core::ptr::write_volatile(virt_addr, pm1b_value);
        }
    }

    Ok(())
}

/// QEMU 特定关机方法
///
/// QEMU 的 virt 机器类型通常通过特定的内存映射寄存器来处理关机。
/// 对于 LoongArch64,通常在 MMIO 区域有一个测试设备。
fn qemu_shutdown() -> Result<(), ShutdownError> {
    // QEMU LoongArch64 virt 机器的关机寄存器地址
    // 根据日志,MMIO 区域从 0x1d000000 开始
    // 尝试使用系统重置控制器
    //
    // 参考: QEMU 源码 hw/loongarch/virt.c
    // LoongArch 使用类似 ARM 的关机机制

    // 常见的 QEMU 关机地址 (需要在 MMIO 范围内):
    // - 0x1fe00000 (尝试失败)
    // - 0x1d000000 + offset (MMIO 区域)

    // 暂时返回错误,让系统进入 idle 循环
    // TODO: 找到正确的 QEMU LoongArch64 关机寄存器地址
    error!("QEMU shutdown not implemented for LoongArch64 yet");
    Err(ShutdownError::InvalidPm1Address)
}

/// 从 FADT 提取 S5 睡眠类型
///
/// # ACPI 规范说明
///
/// 根据 ACPI 规范,睡眠类型值定义在 DSDT 的 `\_S5` 包中,
/// 但很多固件将 S5 类型编码在 FADT 的固定位置。
///
/// # Linux 内核参考
///
/// Linux 内核通过评估 `\_S5` 对象来获取 S5 类型值。
/// 参考: `drivers/acpi/acpi_platform.c`
///
/// # 实现
///
/// 由于我们还没有完整的 AML 解释器集成,这里使用简化方法:
/// 1. 尝试使用固定的 S5 类型值 (0x7 是最常见的值)
/// 2. 后续可以添加 AML 解释器支持来动态获取
fn get_s5_sleep_type(_fadt: &Fadt) -> Result<(u16, u16), ShutdownError> {
    // S5 睡眠类型值
    // 根据 ACPI 规范和实际硬件经验:
    // - S5 类型通常为 0x7 (Soft Off)
    // - 编码在 PM1_CNT 寄存器的 bit 10-12
    //
    // 参考实现:
    // - Linux: drivers/acpi/power.c
    // - 常见值: S5 = 0x7 (most common)
    //
    // TODO: 未来可以通过 AML 解释器从 \_S5 对象获取准确的类型值
    const S5_TYP: u16 = 0x7 << 10; // S5 类型值,左移 10 位到正确位置

    // 返回 S5 类型 (PM1a 和 PM1b 通常相同)
    Ok((S5_TYP, S5_TYP))
}
