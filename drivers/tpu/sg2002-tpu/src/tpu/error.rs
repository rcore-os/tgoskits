//! TPU 错误类型定义

/// TPU 操作错误类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TpuError {
    /// 超时错误
    #[error("TPU operation timed out")]
    Timeout,
    /// 无效的 DMA buffer
    #[error("invalid TPU DMA buffer")]
    InvalidDmabuf,
    /// TDMA 错误
    #[error("TPU TDMA error: {0:#x}")]
    TdmaError(u32),
    /// TIU 错误
    #[error("TPU TIU error: {0:#x}")]
    TiuError(u32),
    /// 设备未初始化
    #[error("TPU is not initialized")]
    NotInitialized,
    /// 设备正忙
    #[error("TPU is busy")]
    Busy,
    /// 被中断
    #[error("TPU operation was interrupted")]
    Interrupted,
    /// PMU buffer 地址未对齐
    #[error("TPU PMU buffer address is not aligned")]
    PmuBufferNotAligned,
    /// DMA buffer 地址未对齐
    #[error("TPU DMA buffer address is not aligned")]
    DmabufNotAligned,
}
