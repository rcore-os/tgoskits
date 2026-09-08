use bitflags::bitflags;

use crate::interface::{BufType, Field, Timecode, Timeval, format::Format};

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BufCapabilities: u32 {
        const SUPPORTS_MMAP = 1 << 0;
        const SUPPORTS_USERPTR = 1 << 1;
        const SUPPORTS_DMABUF = 1 << 2;
        const SUPPORTS_REQUESTS = 1 << 3;
        const SUPPORTS_ORPHANED_BUFS = 1 << 4;
        const SUPPORTS_M2M_HOLD_CAPTURE_BUF = 1 << 5;
        const SUPPORTS_MMAP_CACHE_HINTS = 1 << 6;
        const SUPPORTS_MAX_NUM_BUFFERS = 1 << 7;
        const SUPPORTS_REMOVE_BUFS = 1 << 8;
    }
}

/// 缓冲区的内存映射类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Memory {
    Mmap    = 1,
    Userptr = 2,
    Overlay = 3,
    Dmabuf  = 4,
}

// ========================================================================
// v4l2_requestbuffers（请求缓冲区）
// ========================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Requestbuffers {
    pub count: u32,                    // [inout] 输入：请求的数量，输出：实际数量
    pub ty: BufType,                   // [in] 缓冲区类型
    pub memory: Memory,                // [in] 内存映射类型
    pub capabilities: BufCapabilities, // [out] 缓冲区能力
    pub flags: u8,                     // [in] 请求标志
    pub reserved: [u8; 3],
}

// ========================================================================
// v4l2_exportbuffer（导出缓冲区）
// ========================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Exportbuffer {
    pub ty: BufType, // [in] 缓冲区类型
    pub index: u32,  // [in] 缓冲区索引
    pub plane: u32,  // [in] plane 索引
    pub flags: u32,  // [in] 导出标志
    pub fd: i32,     // [out] 文件描述符
    pub reserved: [u32; 11],
}

// ========================================================================
// v4l2_create_buffers（创建缓冲区）
// ========================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CreateBuffers {
    pub index: u32,                    // [out] 第一个创建的缓冲区索引
    pub count: u32,                    // [inout] 输入：请求的数量，输出：实际数量
    pub memory: Memory,                // [in] 内存映射类型
    pub format: Format,                // [in] 缓冲区格式
    pub capabilities: BufCapabilities, // [out] 缓冲区能力
    pub flags: u32,                    // [in] 请求标志
    pub max_num_buffers: u32,          // [in] 最大缓冲区数量
    pub reserved: [u32; 5],
}

// ========================================================================
// v4l2_remove_buffers（移除缓冲区）
// ========================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RemoveBuffers {
    pub index: u32,  // [in] 要移除的第一个缓冲区索引
    pub count: u32,  // [in] 要移除的缓冲区数量
    pub ty: BufType, // [in] 缓冲区类型
    pub reserved: [u32; 13],
}

// ========================================================================
// v4l2_buffer — 核心缓冲区描述符
// ========================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub union BufferM {
    pub offset: u32,
    pub userptr: usize,
    pub fd: i32,
}

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BufFlags: u32 {
        const MAPPED = 1 << 0;
        const QUEUED = 1 << 1;
        const DONE = 1 << 2;
        const KEYFRAME = 1 << 3;
        const PFRAME = 1 << 4;
        const BFRAME = 1 << 5;
        const ERROR = 1 << 6;
        const IN_REQUEST = 1 << 7;
        const TIMECODE = 1 << 8;
        const M2M_HOLD_CAPTURE_BUF = 1 << 9;
        const PREPARED = 1 << 10;
        const NO_CACHE_INVALIDATE = 1 << 11;
        const NO_CACHE_CLEAN = 1 << 12;
        const TIMESTAMP_MONOTONIC = 1 << 13;
        const LAST = 1 << 16;
        const REQUEST_FD = 1 << 17;
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Buffer {
    pub index: u32,         // [inout] 缓冲区索引，仅 VIDIOC_DQBUF 时为输出
    pub ty: BufType,        // [in] 缓冲区类型
    pub bytesused: u32,     // [out] 缓冲区中数据占用的字节数
    pub flags: BufFlags,    // [out] 缓冲区标志
    pub field: Field,       // [out] 缓冲区中图像的场顺序
    pub timestamp: Timeval, // [out] 缓冲区时间戳
    pub timecode: Timecode, // [out] 缓冲区时间码
    pub sequence: u32,      // [out] 缓冲区序列号
    pub memory: Memory,     // [in] 内存映射类型
    pub m: BufferM,         // [inout] 内存位置（offset / userptr / fd）
    pub length: u32,        // [out] 缓冲区长度（字节）
    pub reserved2: u32,
    pub request_fd: i32, // [in] 请求文件描述符
}

// ========================================================================
// 缓冲区标志（v4l2_buffer.flags）
// ========================================================================

/// 多平面（multi-planar）缓冲区的 plane 信息。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Plane {
    pub bytesused: u32,   // [out] plane 中数据占用的字节数
    pub length: u32,      // [in] plane 长度（字节）
    pub m: PlaneM,        // [in] 内存位置（mem_offset / userptr / fd）
    pub data_offset: u32, // [in] 到实际数据的字节偏移
    pub reserved: [u32; 11],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union PlaneM {
    pub mem_offset: u32,
    pub userptr: usize,
    pub fd: i32,
}
