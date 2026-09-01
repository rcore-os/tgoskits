//! 调试寄存器结构。

use bitflags::bitflags;

/// 芯片匹配类型 — `V4L2_CHIP_MATCH_*`。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipMatch {
    Bridge = 0,
    Subdev = 4,
}

impl ChipMatch {
    /// 尝试将原始 `u32` 转换为 [`ChipMatch`]。
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0 => Self::Bridge,
            4 => Self::Subdev,
            _ => return None,
        })
    }
}

/// 芯片匹配值联合。
#[repr(C)]
#[derive(Clone, Copy)]
pub union DbgMatchValue {
    pub addr: u32,
    pub name: [u8; 32],
}

impl core::fmt::Debug for DbgMatchValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 拷贝到局部变量以避免对 packed 结构字段的未对齐引用。
        let addr = unsafe { self.addr };
        f.debug_tuple("DbgMatchValue").field(&addr).finish()
    }
}

/// 芯片匹配信息。
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct DbgMatch {
    pub ty: ChipMatch, // [in] 匹配类型
    pub value: DbgMatchValue,
}

impl core::fmt::Debug for DbgMatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 拷贝到局部变量以避免对 packed 结构字段的未对齐引用。
        let ty = self.ty;
        let addr = unsafe { self.value.addr };
        f.debug_struct("DbgMatch")
            .field("ty", &ty)
            .field("addr", &addr)
            .finish()
    }
}

/// 调试寄存器访问。
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct DbgRegister {
    pub match_: DbgMatch, // [in] 芯片匹配
    pub size: u32,        // [in] 寄存器大小（字节）
    pub reg: u64,         // [in] 寄存器地址
    pub val: u64,         // [inout] 寄存器值
}

bitflags! {
    /// 芯片标志 — `V4L2_CHIP_FL_*`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ChipFlags: u32 {
        const READABLE  = 1 << 0;
        const WRITABLE  = 1 << 1;
    }
}

/// 芯片信息。
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct DbgChipInfo {
    pub match_: DbgMatch, // [in] 芯片匹配
    pub name: [u8; 32],   // [out] 芯片名称
    pub flags: ChipFlags, // [out] 芯片能力
    pub reserved: [u32; 32],
}
