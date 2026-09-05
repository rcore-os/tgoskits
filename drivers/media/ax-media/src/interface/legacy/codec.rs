//! 编码器/解码器命令结构。

use bitflags::bitflags;

// ── 编码器索引 ────────────────────────────────────────────────────

pub const ENC_IDX_ENTRIES: usize = 64;

/// 编码器索引条目。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EncIndexEntry {
    pub offset: u64, // [out] 帧在流中的偏移
    pub pts: u64,    // [out] 帧的 PTS 时间戳
    pub length: u32, // [out] 帧长度
    pub flags: u32,  // [out] 帧类型（EncIdxFrame 值）
    pub reserved: [u32; 2],
}

/// 编码器索引帧类型 — `V4L2_ENC_IDX_FRAME_*`。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncIdxFrame {
    I = 0,
    P = 1,
    B = 2,
}

/// 编码器索引。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EncIndex {
    pub entries: u32,     // [out] 条目数量
    pub entries_cap: u32, // [out] 条目容量
    pub reserved: [u32; 4],
    pub entry: [EncIndexEntry; ENC_IDX_ENTRIES],
}

// ── 编码器命令 ────────────────────────────────────────────────────

/// 编码器命令 — `V4L2_ENC_CMD_*`。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncCmd {
    Start  = 0,
    Stop   = 1,
    Pause  = 2,
    Resume = 3,
}

bitflags! {
    /// 编码器命令标志 — `V4L2_ENC_CMD_STOP_*`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EncCmdFlags: u32 {
        const STOP_AT_GOP_END = 1 << 0;
    }
}

/// 编码器命令。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EncoderCmd {
    pub cmd: EncCmd,        // [in] 命令
    pub flags: EncCmdFlags, // [in] 命令标志
    pub data: [u32; 8],     // [inout] 命令数据
}

// ── 解码器命令 ────────────────────────────────────────────────────

/// 解码器命令 — `V4L2_DEC_CMD_*`。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecCmd {
    Start  = 0,
    Stop   = 1,
    Pause  = 2,
    Resume = 3,
    Flush  = 4,
}

bitflags! {
    /// 解码器命令标志 — `V4L2_DEC_CMD_START_*`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DecStartFlags: u32 {
        const MUTE_AUDIO = 1 << 0;
    }
}

bitflags! {
    /// 解码器命令标志 — `V4L2_DEC_CMD_PAUSE_*`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DecPauseFlags: u32 {
        const TO_BLACK = 1 << 0;
    }
}

bitflags! {
    /// 解码器命令标志 — `V4L2_DEC_CMD_STOP_*`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DecStopFlags: u32 {
        const TO_BLACK     = 1 << 0;
        const IMMEDIATELY  = 1 << 1;
    }
}

/// 解码器 STOP 参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DecStop {
    pub pts: u64,
}

/// 解码器 START 参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DecStart {
    pub speed: i32,
    pub format: u32,
}

/// 解码器载荷联合。
#[repr(C)]
#[derive(Clone, Copy)]
pub union DecoderPayload {
    pub stop: DecStop,
    pub start: DecStart,
    pub raw: [u32; 16],
}

impl core::fmt::Debug for DecoderPayload {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 按原始 64 字节打印，不解引用具体语义字段。
        f.debug_tuple("DecoderPayload")
            .field(&unsafe { self.raw })
            .finish()
    }
}

/// 解码器命令。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DecoderCmd {
    pub cmd: DecCmd,             // [in] 命令
    pub flags: u32,              // [in] 命令标志（按命令解释为对应位集）
    pub payload: DecoderPayload, // [inout] 命令载荷
}
