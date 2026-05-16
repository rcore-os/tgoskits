//! TPU 错误类型定义

/// TPU 操作错误类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpuError {
    /// 超时错误
    Timeout,
    /// 无效的 DMA buffer
    InvalidDmabuf,
    /// TDMA 错误
    TdmaError(u32),
    /// TIU 错误
    TiuError(u32),
    /// 设备未初始化
    NotInitialized,
    /// 设备正忙
    Busy,
    /// 被中断
    Interrupted,
    /// PMU buffer 地址未对齐
    PmuBufferNotAligned,
    /// DMA buffer 地址未对齐
    DmabufNotAligned,
}

impl TpuError {
    /// 获取错误码 (兼容 Linux errno 风格)
    pub fn as_errno(&self) -> i32 {
        match self {
            TpuError::Timeout => -110,       // ETIMEDOUT
            TpuError::InvalidDmabuf => -22,  // EINVAL
            TpuError::TdmaError(_) => -5,    // EIO
            TpuError::TiuError(_) => -5,     // EIO
            TpuError::NotInitialized => -19, // ENODEV
            TpuError::Busy => -16,           // EBUSY
            TpuError::Interrupted => -4,     // EINTR
            TpuError::PmuBufferNotAligned => -22,
            TpuError::DmabufNotAligned => -22,
        }
    }
}
