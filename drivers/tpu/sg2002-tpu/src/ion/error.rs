//! Ion 驱动错误类型定义

/// Ion 驱动错误类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IonError {
    /// 无效参数
    #[error("invalid argument")]
    InvalidArg,
    /// 内存不足
    #[error("out of memory")]
    NoMemory,
    /// 无效的缓冲区句柄
    #[error("invalid buffer handle")]
    InvalidBuffer,
    /// 缓冲区已存在
    #[error("buffer already exists")]
    BufferExists,
    /// 缓冲区未找到
    #[error("buffer not found")]
    BufferNotFound,
    /// 无效的堆类型
    #[error("invalid heap type")]
    InvalidHeap,
    /// 操作不支持
    #[error("operation not supported")]
    NotSupported,
    /// 内部错误
    #[error("internal error")]
    Internal,
}

pub type IonResult<T> = Result<T, IonError>;
