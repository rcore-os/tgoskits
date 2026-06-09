//! SDIO 错误类型  

/// SDIO 操作错误  
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdioError {
    /// 命令或数据传输超时  
    Timeout,
    /// CRC 校验失败  
    CrcError,
    /// 不支持的操作  
    Unsupported,
    /// 通用 IO 错误  
    IoError,
}

impl core::fmt::Display for SdioError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Timeout => write!(f, "SDIO timeout"),
            Self::CrcError => write!(f, "SDIO CRC error"),
            Self::Unsupported => write!(f, "SDIO unsupported operation"),
            Self::IoError => write!(f, "SDIO I/O error"),
        }
    }
}
