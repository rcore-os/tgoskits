#[cfg(target_arch = "aarch64")]
#[allow(dead_code)]
use core::arch::asm;

#[cfg(target_arch = "aarch64")]
lazy_static::lazy_static! {
    #[allow(dead_code)]
    pub static ref HARDWARE_SUPPORT_CRC32: bool = has_hardware_crc32();
}

// 在 Rust core::arch::aarch64 中，带 'c' 后缀的才是 Castagnoli

#[cfg(target_arch = "aarch64")]
#[allow(dead_code)]
use core::arch::aarch64::{__crc32cb, __crc32cd, __crc32ch, __crc32cw};
/// 检查 CPU 是否支持 CRC32 硬件加速
/// 读取 ID_AA64ISAR0_EL1 寄存器的 [19:16] 位
#[cfg(target_arch = "aarch64")]
#[inline]
#[allow(dead_code)]
pub fn has_hardware_crc32() -> bool {
    use log::warn;

    let mut reg_val: u64;
    unsafe {
        // mrs: Move from System Register to general purpose register
        asm!("mrs {}, ID_AA64ISAR0_EL1", out(reg) reg_val);
    }

    // Bits [19:16] 代表 CRC32 支持情况
    let crc_field = (reg_val >> 16) & 0xF;
    warn!("ID_AA64ISAR0_EL1[19:16]: {:#x}", crc_field);
    // 0x1 代表支持 CRC32/CRC32C
    if crc_field >= 1 {
        warn!("Hardware CRC32C support detected.");
        true
    } else {
        warn!("No hardware CRC32C support.");
        false
    }
}

/// 使用硬件指令计算 CRC32C (Castagnoli)
/// 自动处理 64位/32位/16位/8位 对齐以达到最大吞吐量
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "crc")] // 告诉编译器在这个函数里可以使用 crc 特性
#[inline]
#[allow(dead_code)]
pub unsafe fn crc32c_hardware(mut crc: u32, data: &[u8]) -> u32 {
    // Ext4 这里的 crc 通常需要取反传入 (!crc)，但这取决于你的调用层逻辑
    // 这里的实现只负责单纯的 update
    unsafe {
        let mut p = data.as_ptr();
        let mut len = data.len();

        // 1. 处理头部不对齐的部分 (Alignment to 8 bytes)
        // 这里的目的是为了后续能由 u64 (8字节) 快速处理
        while len > 0 && (p as usize) % 8 != 0 {
            crc = __crc32cb(crc, *p);
            p = p.add(1);
            len -= 1;
        }

        // 2. 核心循环：一次处理 64 bits (8 bytes)
        // 这是最快的部分，现代 CPU 一个周期能吞吐 64 位
        while len >= 8 {
            // 读取 64 位数据
            let val = *(p as *const u64);
            crc = __crc32cd(crc, val);
            p = p.add(8);
            len -= 8;
        }

        // 3. 处理剩余不足 8 字节的尾部
        // 降级处理: 4字节 -> 2字节 -> 1字节
        if len >= 4 {
            let val = *(p as *const u32);
            crc = __crc32cw(crc, val);
            p = p.add(4);
            len -= 4;
        }

        if len >= 2 {
            let val = *(p as *const u16);
            crc = __crc32ch(crc, val);
            p = p.add(2);
            len -= 2;
        }

        if len > 0 {
            crc = __crc32cb(crc, *p);
        }

        crc
    }
}
