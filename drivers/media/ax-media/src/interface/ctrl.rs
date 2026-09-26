//! 控制（Control）结构与常量。
//!
//! Control、ExtControl、ExtControls、
//! QueryCtrl、QueryExtCtrl、Querymenu 以及 Control 标志。

use bitflags::bitflags;

/// 简单控制（id + value）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Control {
    pub id: u32,    // [in] 控制 ID
    pub value: i32, // [inout] 控制值（G_CTRL：输出，S_CTRL：输入）
}

/// 扩展控制。
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct ExtControl {
    pub id: u32,   // [in] 控制 ID
    pub size: u32, // [inout] 值的大小（字符串 / 复合类型）
    pub reserved2: [u32; 1],
    /// value/value64/string/ptr 等的联合体。
    pub value: ExtControlValue, // [inout] 控制值
}

/// 扩展控制值联合体。
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub union ExtControlValue {
    pub value: i32,
    pub value64: i64,
    pub string: usize,
    pub ptr: usize,
}

impl core::fmt::Debug for ExtControlValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let v = unsafe { core::ptr::addr_of!(self.value).read_unaligned() };
        f.debug_struct("ExtControlValue")
            .field("value", &v)
            .finish()
    }
}

/// 扩展控制容器。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ExtControls {
    /// ctrl_class（用户态）与 which 的联合
    pub which: u32, // [in] 哪一组控制
    pub count: u32,      // [inout] 控制数量（输出：实际数量 / 错误索引基准）
    pub error_idx: u32,  // [out] 第一个失败控制的索引
    pub request_fd: i32, // [in] 请求文件描述符
    pub reserved: [u32; 1],
    pub controls: usize, // [in] 指向控制数组的用户态指针
}

// 控制 ID 辅助常量
pub const CTRL_ID_MASK: u32 = 0x0FFF_FFFF;
pub const CTRL_MAX_DIMS: u32 = 4;
pub const CTRL_WHICH_CUR_VAL: u32 = 0;
pub const CTRL_WHICH_DEF_VAL: u32 = 0x0F00_0000;
pub const CTRL_WHICH_REQUEST_VAL: u32 = 0x0F01_0000;
pub const CTRL_WHICH_MIN_VAL: u32 = 0x0F02_0000;
pub const CTRL_WHICH_MAX_VAL: u32 = 0x0F03_0000;

// 控制标志
bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CtrlFlags: u32 {
        const DISABLED = 0x0001;
        const GRABBED = 0x0002;
        const READ_ONLY = 0x0004;
        const UPDATE = 0x0008;
        const INACTIVE = 0x0010;
        const SLIDER = 0x0020;
        const WRITE_ONLY = 0x0040;
        const VOLATILE = 0x0080;
        const HAS_PAYLOAD = 0x0100;
        const EXECUTE_ON_WRITE = 0x0200;
        const MODIFY_LAYOUT = 0x0400;
        const DYNAMIC_ARRAY = 0x0800;
        const HAS_WHICH_MIN_MAX = 0x1000;
    }
}

/// 简单查询控制结果。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct QueryCtrl {
    pub id: u32,            // [in] 控制 ID
    pub ty: u32,            // [out] 控制类型
    pub name: [u8; 32],     // [out] 控制名称
    pub minimum: i32,       // [out] 最小值
    pub maximum: i32,       // [out] 最大值
    pub step: i32,          // [out] 步长
    pub default_value: i32, // [out] 默认值
    pub flags: CtrlFlags,   // [out] 控制标志
    pub reserved: [u32; 2],
}

/// 扩展查询控制结果。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct QueryExtCtrl {
    pub id: u32,                             // [in] 控制 ID
    pub ty: u32,                             // [out] 控制类型
    pub name: [u8; 32],                      // [out] 控制名称
    pub minimum: i64,                        // [out] 最小值
    pub maximum: i64,                        // [out] 最大值
    pub step: u64,                           // [out] 步长
    pub default_value: i64,                  // [out] 默认值
    pub flags: CtrlFlags,                    // [out] 控制标志
    pub elem_size: u32,                      // [out] 元素大小（字节）
    pub elems: u32,                          // [out] 元素数量
    pub nr_of_dims: u32,                     // [out] 维度数量
    pub dims: [u32; CTRL_MAX_DIMS as usize], // [out] 各维度大小
    pub reserved: [u32; 32],
}

/// 查询菜单项结果。
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Querymenu {
    pub id: u32,    // [in] 控制 ID
    pub index: u32, // [in] 菜单项索引
    /// name[32] 与 value（s64）的联合
    pub name: [u8; 32], // [out] 菜单项名称 / 值
    pub reserved: u32,
}

// 用户类控制上限
pub const CID_MAX_CTRLS: u32 = 1024;
pub const CID_PRIVATE_BASE: u32 = 0x0800_0000;
