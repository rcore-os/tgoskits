//! Pull/Drive/Schmitt 寄存器映射表
//!
//! 从 u-boot 提取的静态寄存器映射表。

use crate::{PinId, pinctrl::id::*};

/// Pull 寄存器条目
#[derive(Debug, Clone, Copy)]
pub struct PullEntry {
    /// 引脚 ID
    pub pin_id: PinId,
    /// 寄存器偏移（相对 IOC 基地址）
    pub reg_offset: usize,
}

impl PullEntry {
    const fn new(pin_id: PinId, reg_offset: usize) -> Self {
        Self { pin_id, reg_offset }
    }
}

/// Drive Strength 寄存器条目
#[derive(Debug, Clone, Copy)]
pub struct DriveEntry {
    /// 引脚 ID
    pub pin_id: PinId,
    /// 寄存器偏移（相对 IOC 基地址）
    pub reg_offset: usize,
}

impl DriveEntry {
    const fn new(pin_id: PinId, reg_offset: usize) -> Self {
        Self { pin_id, reg_offset }
    }
}

/// Schmitt Trigger 寄存器条目
#[derive(Debug, Clone, Copy)]
pub struct SchmittEntry {
    /// 引脚 ID
    pub pin_id: PinId,
    /// 寄存器偏移（相对 IOC 基地址）
    pub reg_offset: usize,
}

impl SchmittEntry {
    const fn new(pin_id: PinId, reg_offset: usize) -> Self {
        Self { pin_id, reg_offset }
    }
}

// 从 u-boot rk3588_ds_regs[] 提取
const DRIVE_REGS: &[DriveEntry] = &[
    // GPIO0
    DriveEntry::new(GPIO0_A0, 0x0010), // GPIO0_A0
    DriveEntry::new(GPIO0_A4, 0x0014), // GPIO0_A4
    DriveEntry::new(GPIO0_B0, 0x0018), // GPIO0_B0
    DriveEntry::new(GPIO0_B4, 0x4014), // GPIO0_B4 (PMU2_IOC)
    DriveEntry::new(GPIO0_C0, 0x4018), // GPIO0_C0 (PMU2_IOC)
    DriveEntry::new(GPIO0_C4, 0x401C), // GPIO0_C4 (PMU2_IOC)
    DriveEntry::new(GPIO0_D0, 0x4020), // GPIO0_D0 (PMU2_IOC)
    DriveEntry::new(GPIO0_D4, 0x4024), // GPIO0_D4 (PMU2_IOC)
    // GPIO1
    DriveEntry::new(GPIO1_A0, 0x9020), // GPIO1_A0 (VCCIO1-4_IOC)
    DriveEntry::new(GPIO1_A4, 0x9024), // GPIO1_A4
    DriveEntry::new(GPIO1_B0, 0x9028), // GPIO1_B0
    DriveEntry::new(GPIO1_B4, 0x902C), // GPIO1_B4
    DriveEntry::new(GPIO1_C0, 0x9030), // GPIO1_C0
    DriveEntry::new(GPIO1_C4, 0x9034), // GPIO1_C4
    DriveEntry::new(GPIO1_D0, 0x9038), // GPIO1_D0
    DriveEntry::new(GPIO1_D4, 0x903C), // GPIO1_D4
    // GPIO2
    DriveEntry::new(GPIO2_A0, 0xD040), // GPIO2_A0 (EMMC_IOC)
    DriveEntry::new(GPIO2_A4, 0xA044), // GPIO2_A4 (VCCIO3-5_IOC)
    DriveEntry::new(GPIO2_B0, 0xA048), // GPIO2_B0 (VCCIO3-5_IOC)
    DriveEntry::new(GPIO2_B4, 0xA04C), // GPIO2_B4 (VCCIO3-5_IOC)
    DriveEntry::new(GPIO2_C0, 0xA050), // GPIO2_C0 (VCCIO3-5_IOC)
    DriveEntry::new(GPIO2_C4, 0xA054), // GPIO2_C4 (VCCIO3-5_IOC)
    DriveEntry::new(GPIO2_D0, 0xD058), // GPIO2_D0 (EMMC_IOC)
    DriveEntry::new(GPIO2_D4, 0xD05C), // GPIO2_D4 (EMMC_IOC)
    // GPIO3
    DriveEntry::new(GPIO3_A0, 0xA060), // GPIO3_A0 (VCCIO3-5_IOC)
    DriveEntry::new(GPIO3_A4, 0xA064), // GPIO3_A4
    DriveEntry::new(GPIO3_B0, 0xA068), // GPIO3_B0
    DriveEntry::new(GPIO3_B4, 0xA06C), // GPIO3_B4
    DriveEntry::new(GPIO3_C0, 0xA070), // GPIO3_C0
    DriveEntry::new(GPIO3_C4, 0xA074), // GPIO3_C4
    DriveEntry::new(GPIO3_D0, 0xA078), // GPIO3_D0
    DriveEntry::new(GPIO3_D4, 0xA07C), // GPIO3_D4
    // GPIO4
    DriveEntry::new(GPIO4_A0, 0xC080), // GPIO4_A0 (VCCIO6_IOC)
    DriveEntry::new(GPIO4_A4, 0xC084), // GPIO4_A4
    DriveEntry::new(GPIO4_B0, 0xC088), // GPIO4_B0
    DriveEntry::new(GPIO4_B4, 0xC08C), // GPIO4_B4
    DriveEntry::new(GPIO4_C0, 0xC090), // GPIO4_C0
    DriveEntry::new(GPIO4_C2, 0xA090), // GPIO4_C2 (VCCIO3-5_IOC)
    DriveEntry::new(GPIO4_C4, 0xA094), // GPIO4_C4 (VCCIO3-5_IOC)
    DriveEntry::new(GPIO4_D0, 0xB098), // GPIO4_D0 (VCCIO2_IOC)
];

// 从 u-boot rk3588_p_regs[] 提取
const PULL_REGS: &[PullEntry] = &[
    // GPIO0
    PullEntry::new(GPIO0_A0, 0x0020), // GPIO0_A0
    PullEntry::new(GPIO0_B0, 0x0024), // GPIO0_B0
    PullEntry::new(GPIO0_B5, 0x4028), // GPIO0_B5 (PMU2_IOC)
    PullEntry::new(GPIO0_C0, 0x402C), // GPIO0_C0 (PMU2_IOC)
    PullEntry::new(GPIO0_D0, 0x4030), // GPIO0_D0 (PMU2_IOC)
    // GPIO1
    PullEntry::new(GPIO1_A0, 0x9110), // GPIO1_A0 (VCCIO1-4_IOC)
    PullEntry::new(GPIO1_B0, 0x9114), // GPIO1_B0
    PullEntry::new(GPIO1_C0, 0x9118), // GPIO1_C0
    PullEntry::new(GPIO1_D0, 0x911C), // GPIO1_D0
    // GPIO2
    PullEntry::new(GPIO2_A0, 0xD120), // GPIO2_A0 (EMMC_IOC)
    PullEntry::new(GPIO2_A4, 0xA120), // GPIO2_A4 (VCCIO3-5_IOC)
    PullEntry::new(GPIO2_B0, 0xA124), // GPIO2_B0 (VCCIO3-5_IOC)
    PullEntry::new(GPIO2_C0, 0xA128), // GPIO2_C0 (VCCIO3-5_IOC)
    PullEntry::new(GPIO2_D0, 0xD12C), // GPIO2_D0 (EMMC_IOC)
    // GPIO3
    PullEntry::new(GPIO3_A0, 0xA130), // GPIO3_A0 (VCCIO3-5_IOC)
    PullEntry::new(GPIO3_B0, 0xA134), // GPIO3_B0
    PullEntry::new(GPIO3_C0, 0xA138), // GPIO3_C0
    PullEntry::new(GPIO3_D0, 0xA13C), // GPIO3_D0
    // GPIO4
    PullEntry::new(GPIO4_A0, 0xC140), // GPIO4_A0 (VCCIO6_IOC)
    PullEntry::new(GPIO4_B0, 0xC144), // GPIO4_B0
    PullEntry::new(GPIO4_C0, 0xC148), // GPIO4_C0
    PullEntry::new(GPIO4_C2, 0xA148), // GPIO4_C2 (VCCIO3-5_IOC)
    PullEntry::new(GPIO4_D0, 0xB14C), // GPIO4_D0 (VCCIO2_IOC)
];

// 从 u-boot rk3588_smt_regs[] 提取
#[allow(dead_code)]
const SCHMITT_REGS: &[SchmittEntry] = &[
    // GPIO0
    SchmittEntry::new(GPIO0_A0, 0x0030), // GPIO0_A0
    SchmittEntry::new(GPIO0_B0, 0x0034), // GPIO0_B0
    SchmittEntry::new(GPIO0_B5, 0x4040), // GPIO0_B5 (PMU2_IOC)
    SchmittEntry::new(GPIO0_C0, 0x4044), // GPIO0_C0 (PMU2_IOC)
    SchmittEntry::new(GPIO0_D0, 0x4048), // GPIO0_D0 (PMU2_IOC)
    // GPIO1
    SchmittEntry::new(GPIO1_A0, 0x9210), // GPIO1_A0 (VCCIO1-4_IOC)
    SchmittEntry::new(GPIO1_B0, 0x9214), // GPIO1_B0
    SchmittEntry::new(GPIO1_C0, 0x9218), // GPIO1_C0
    SchmittEntry::new(GPIO1_D0, 0x921C), // GPIO1_D0
    // GPIO2
    SchmittEntry::new(GPIO2_A0, 0xD220), // GPIO2_A0 (EMMC_IOC)
    SchmittEntry::new(GPIO2_A4, 0xA220), // GPIO2_A4 (VCCIO3-5_IOC)
    SchmittEntry::new(GPIO2_B0, 0xA224), // GPIO2_B0 (VCCIO3-5_IOC)
    SchmittEntry::new(GPIO2_C0, 0xA228), // GPIO2_C0 (VCCIO3-5_IOC)
    SchmittEntry::new(GPIO2_D0, 0xD22C), // GPIO2_D0 (EMMC_IOC)
    // GPIO3
    SchmittEntry::new(GPIO3_A0, 0xA230), // GPIO3_A0 (VCCIO3-5_IOC)
    SchmittEntry::new(GPIO3_B0, 0xA234), // GPIO3_B0
    SchmittEntry::new(GPIO3_C0, 0xA238), // GPIO3_C0
    SchmittEntry::new(GPIO3_D0, 0xA23C), // GPIO3_D0
    // GPIO4
    SchmittEntry::new(GPIO4_A0, 0xC240), // GPIO4_A0 (VCCIO6_IOC)
    SchmittEntry::new(GPIO4_B0, 0xC244), // GPIO4_B0
    SchmittEntry::new(GPIO4_C0, 0xC248), // GPIO4_C0
    SchmittEntry::new(GPIO4_C2, 0xA248), // GPIO4_C2 (VCCIO3-5_IOC)
    SchmittEntry::new(GPIO4_D0, 0xB24C), // GPIO4_D0 (VCCIO2_IOC)
];

/// 查找 drive strength 寄存器配置
///
/// # 参数
///
/// * `pin` - 引脚 ID
///
/// # 返回
///
/// 返回 `(寄存器偏移, 位偏移)`，如果引脚无效则返回 `None`
///
/// # 参考
///
/// u-boot: `drivers/pinctrl/rockchip/pinctrl-rk3588.c:rk3588_calc_drv_reg_and_bit`
pub fn find_drive_entry(pin: PinId) -> Option<(usize, u32)> {
    let pin_num = pin.raw();

    // 查找寄存器条目（从后向前查找，找到第一个 pin_id <= pin_num 的）
    let entry = DRIVE_REGS
        .iter()
        .rev()
        .find(|e| e.pin_id.raw() <= pin_num)?;

    // 计算相对于条目起始引脚的偏移
    let pin_offset = pin_num - entry.pin_id.raw();

    // 计算寄存器偏移增量（每4个引脚一个寄存器，占4字节）
    // 参考 u-boot: *reg += ((pin - rk3588_ds_regs[i][0]) / RK3588_DRV_PINS_PER_REG) * 4;
    const DRV_PINS_PER_REG: u32 = 4;
    let reg_offset = entry.reg_offset + (pin_offset / DRV_PINS_PER_REG) as usize * 4;

    // 计算位偏移（使用 pin_in_bank，每4个引脚循环，每个引脚4位）
    // 参考 u-boot: *bit = pin_num % RK3588_DRV_PINS_PER_REG; *bit *= RK3588_DRV_BITS_PER_PIN;
    const DRV_BITS_PER_PIN: u32 = 4;
    let bit_offset = (pin.pin_in_bank() % DRV_PINS_PER_REG) * DRV_BITS_PER_PIN;

    Some((reg_offset, bit_offset))
}

/// 查找 pull 寄存器配置
///
/// # 参数
///
/// * `pin` - 引脚 ID
///
/// # 返回
///
/// 返回 `(寄存器偏移, 位偏移)`，如果引脚无效则返回 `None`
///
/// # 参考
///
/// u-boot: `drivers/pinctrl/rockchip/pinctrl-rk3588.c:rk3588_calc_pull_reg_and_bit`
pub fn find_pull_entry(pin: PinId) -> Option<(usize, u32)> {
    let pin_num = pin.raw();

    // 查找寄存器条目
    let entry = PULL_REGS.iter().rev().find(|e| e.pin_id.raw() <= pin_num)?;

    // 计算相对于条目起始引脚的偏移
    let pin_offset = pin_num - entry.pin_id.raw();

    // 计算寄存器偏移增量（每8个引脚一个寄存器，占4字节）
    // 参考 u-boot: *reg += ((pin - rk3588_p_regs[i][0]) / RK3588_PULL_PINS_PER_REG) * 4;
    const PULL_PINS_PER_REG: u32 = 8;
    let reg_offset = entry.reg_offset + (pin_offset / PULL_PINS_PER_REG) as usize * 4;

    // 计算位偏移（使用 pin_in_bank，每8个引脚循环，每个引脚2位）
    // 参考 u-boot: *bit = pin_num % RK3588_PULL_PINS_PER_REG; *bit *= RK3588_PULL_BITS_PER_PIN;
    const PULL_BITS_PER_PIN: u32 = 2;
    let bit_offset = (pin.pin_in_bank() % PULL_PINS_PER_REG) * PULL_BITS_PER_PIN;

    Some((reg_offset, bit_offset))
}

/// 查找 schmitt trigger 寄存器配置
///
/// # 参数
///
/// * `pin` - 引脚 ID
///
/// # 返回
///
/// 返回 `(寄存器偏移, 位偏移)`，如果引脚无效则返回 `None`
///
/// # 参考
///
/// u-boot: `drivers/pinctrl/rockchip/pinctrl-rk3588.c:rk3588_calc_schmitt_reg_and_bit`
#[allow(dead_code)]
pub fn find_schmitt_entry(pin: PinId) -> Option<(usize, u32)> {
    let pin_num = pin.raw();

    // 查找寄存器条目
    let entry = SCHMITT_REGS
        .iter()
        .rev()
        .find(|e| e.pin_id.raw() <= pin_num)?;

    // 计算相对于条目起始引脚的偏移
    let pin_offset = pin_num - entry.pin_id.raw();

    // 计算寄存器偏移增量（每8个引脚一个寄存器，占4字节）
    // 参考 u-boot: *reg += ((pin - rk3588_smt_regs[i][0]) / RK3588_SMT_PINS_PER_REG) * 4;
    const SMT_PINS_PER_REG: u32 = 8;
    let reg_offset = entry.reg_offset + (pin_offset / SMT_PINS_PER_REG) as usize * 4;

    // 计算位偏移（使用 pin_in_bank，每8个引脚循环，每个引脚1位）
    // 参考 u-boot: *bit = pin_num % RK3588_SMT_PINS_PER_REG;
    let bit_offset = pin.pin_in_bank() % SMT_PINS_PER_REG;

    Some((reg_offset, bit_offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_drive_entry() {
        // GPIO0_A0 (pin 0)
        let pin = PinId::new(0).unwrap();
        let (reg_offset, bit_offset) = find_drive_entry(pin).unwrap();
        assert_eq!(reg_offset, 0x0010);
        assert_eq!(bit_offset, 0);

        // GPIO0_A4 (pin 4)
        let pin = PinId::new(4).unwrap();
        let (reg_offset, bit_offset) = find_drive_entry(pin).unwrap();
        assert_eq!(reg_offset, 0x0014);
        assert_eq!(bit_offset, 0);
    }

    #[test]
    fn test_find_pull_entry() {
        // GPIO0_A0 (pin 0)
        let pin = PinId::new(0).unwrap();
        let (reg_offset, bit_offset) = find_pull_entry(pin).unwrap();
        assert_eq!(reg_offset, 0x0020);
        assert_eq!(bit_offset, 0);

        // GPIO0_B0 (pin 8)
        let pin = PinId::new(8).unwrap();
        let (reg_offset, bit_offset) = find_pull_entry(pin).unwrap();
        assert_eq!(reg_offset, 0x0024);
        assert_eq!(bit_offset, 0);
    }

    #[test]
    fn test_bit_offset_calculation() {
        // Drive: 每 4 个引脚一个寄存器，每个引脚 4 位
        // 参考 u-boot: RK3588_DRV_PINS_PER_REG = 4, RK3588_DRV_BITS_PER_PIN = 4
        let pin = PinId::new(0).unwrap();
        let (_, bit_offset) = find_drive_entry(pin).unwrap();
        assert_eq!(bit_offset, 0); // (0 % 4) * 4 = 0

        let pin = PinId::new(1).unwrap();
        let (_, bit_offset) = find_drive_entry(pin).unwrap();
        assert_eq!(bit_offset, 4); // (1 % 4) * 4 = 4

        // Pull: 每 8 个引脚一个寄存器，每个引脚 2 位
        // 参考 u-boot: RK3588_PULL_PINS_PER_REG = 8, RK3588_PULL_BITS_PER_PIN = 2
        let pin = PinId::new(0).unwrap();
        let (_, bit_offset) = find_pull_entry(pin).unwrap();
        assert_eq!(bit_offset, 0); // (0 % 8) * 2 = 0

        let pin = PinId::new(1).unwrap();
        let (_, bit_offset) = find_pull_entry(pin).unwrap();
        assert_eq!(bit_offset, 2); // (1 % 8) * 2 = 2

        // Schmitt: 每 8 个引脚一个寄存器，每个引脚 1 位
        // 参考 u-boot: RK3588_SMT_PINS_PER_REG = 8, RK3588_SMT_BITS_PER_PIN = 1
        let pin = PinId::new(0).unwrap();
        let (_, bit_offset) = find_schmitt_entry(pin).unwrap();
        assert_eq!(bit_offset, 0); // 0 % 8 = 0

        let pin = PinId::new(7).unwrap();
        let (_, bit_offset) = find_schmitt_entry(pin).unwrap();
        assert_eq!(bit_offset, 7); // 7 % 8 = 7
    }
}
