//! EDID 结构。

/// EDID 数据。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Edid {
    pub pad: u32,         // [in] pad 编号（subdev 节点专用）
    pub start_block: u32, // [in] 起始块号
    pub blocks: u32,      // [inout] 块数量（每块 128 字节）
    pub reserved: [u32; 5],
    pub edid: usize, // [in] 指向 EDID 数据缓冲的用户态指针
}
