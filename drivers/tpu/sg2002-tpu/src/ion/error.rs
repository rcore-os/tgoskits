//! Ion 驱动错误类型定义

use core::fmt;

use ax_errno::AxError;

/// Ion 驱动错误类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IonError {
    /// 无效参数
    InvalidArg,
    /// 内存不足
    NoMemory,
    /// 无效的缓冲区句柄
    InvalidBuffer,
    /// 缓冲区已存在
    BufferExists,
    /// 缓冲区未找到
    BufferNotFound,
    /// 无效的堆类型
    InvalidHeap,
    /// 操作不支持
    NotSupported,
    /// 内部错误
    Internal,
}

impl fmt::Display for IonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArg => write!(f, "Invalid argument"),
            Self::NoMemory => write!(f, "Out of memory"),
            Self::InvalidBuffer => write!(f, "Invalid buffer handle"),
            Self::BufferExists => write!(f, "Buffer already exists"),
            Self::BufferNotFound => write!(f, "Buffer not found"),
            Self::InvalidHeap => write!(f, "Invalid heap type"),
            Self::NotSupported => write!(f, "Operation not supported"),
            Self::Internal => write!(f, "Internal error"),
        }
    }
}

impl From<IonError> for AxError {
    fn from(err: IonError) -> Self {
        match err {
            IonError::InvalidArg => AxError::InvalidInput,
            IonError::NoMemory => AxError::NoMemory,
            IonError::InvalidBuffer | IonError::BufferNotFound => AxError::NotFound,
            IonError::BufferExists => AxError::AlreadyExists,
            IonError::InvalidHeap | IonError::NotSupported => AxError::Unsupported,
            IonError::Internal => AxError::Interrupted,
        }
    }
}

pub type IonResult<T> = Result<T, IonError>;
