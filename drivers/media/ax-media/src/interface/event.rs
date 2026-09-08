//! V4L2 事件类型 — v4l2_event、v4l2_event_subscription。
//!
//! 事件允许用户态订阅来自驱动的异步通知
//! （控制变化、格式变化、信号源变化等）。

use bitflags::bitflags;

use crate::interface::Timespec;

/// V4L2 事件订阅请求。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EventSubscription {
    pub ty: EventType,        // [in] 事件类型
    pub id: u32,              // [in] 关联 ID（例如 [`EventType::Ctrl`] 对应的控制 ID）
    pub flags: EventSubFlags, // [in] 事件订阅标志
    pub reserved: [u32; 5],
}

/// 由用户态出队（dequeue）的 V4L2 事件。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub ty: u32, // [out] 事件类型
    pub pad: u32,
    pub data: [u8; 64],      // [out] 事件负载（union v4l2_event.u，偏移 8）
    pub pending: u32,        // [out] 该类型的待处理事件数量
    pub sequence: u32,       // [out] 单调递增序列号
    pub timestamp: Timespec, // [out] 事件时间戳
    pub id: u32,             // [out] 事件类型相关 ID（例如控制 ID）
    pub reserved: [u32; 8],
}

// ── 事件类型 ───────────────────────────────────────────────────────────

/// V4L2 事件类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    All          = 0,
    Vsync        = 1, // 垂直同步信号事件（Vertical Sync）
    Eos          = 2, // 流结束事件（End Of Stream）
    Ctrl         = 3, // 控件变化事件（Control Change）
    FrameSync    = 4, // 帧同步事件（Frame Sync）
    SourceChange = 5, // 信号源变化事件（Source Change）
    MotionDet    = 6, // 运动检测事件（Motion Detection）
    PrivateStart = 0x0800_0000,
}

impl EventType {
    /// 尝试将原始 `u32` 转换为 [`EventType`]。
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0 => Self::All,
            1 => Self::Vsync,
            2 => Self::Eos,
            3 => Self::Ctrl,
            4 => Self::FrameSync,
            5 => Self::SourceChange,
            6 => Self::MotionDet,
            0x0800_0000 => Self::PrivateStart,
            _ => return None,
        })
    }
}

// ── 事件订阅标志 ─────────────────────────────────────────────

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EventSubFlags: u32 {
        /// 订阅时立即发送初始事件。
        const SEND_INITIAL = 1 << 0;
    }
}

// ── V4L2_EVENT_CTRL 载荷 ─────────────────────────────────────

/// `v4l2_event_ctrl` 载荷（V4L2_EVENT_CTRL 的 `u` 联合体布局）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EventCtrlPayload {
    pub changes: u32,
    pub ty: u32,
    pub value: u64,
    pub flags: u32,
    pub minimum: i32,
    pub maximum: i32,
    pub step: i32,
    pub default_value: i32,
}

impl EventCtrlPayload {
    const BYTES: usize = core::mem::size_of::<Self>();

    /// 从事件负载区读回前 `size_of::<EventCtrlPayload>()` 字节。
    pub fn read_from(ev: &Event) -> Self {
        let mut bytes = [0u8; Self::BYTES];
        bytes.copy_from_slice(&ev.data[..Self::BYTES]);
        unsafe { core::mem::transmute(bytes) }
    }

    /// 按位写入目标字节区前 `size_of::<EventCtrlPayload>()` 字节。
    pub fn write_into(&self, dst: &mut [u8]) {
        debug_assert!(dst.len() >= Self::BYTES);
        let bytes: [u8; Self::BYTES] = unsafe { core::mem::transmute(*self) };
        dst[..Self::BYTES].copy_from_slice(&bytes);
    }
}

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CtrlChange: u32 {
        /// 值发生变化。
        const VALUE = 1 << 0;
        /// 标志位发生变化。
        const FLAGS = 1 << 1;
    }
}
