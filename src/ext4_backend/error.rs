//!错误处理模块
//! 
/// 块设备错误类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockDevError {

    /// 非法输入
    InvalidInput,

    /// 读取错误
    ReadError,

    /// 写入错误
    WriteError,

    /// 块号超出范围
    BlockOutOfRange { block_id: u32, max_blocks: u64 },

    /// 无效的块大小
    InvalidBlockSize { size: usize, expected: usize },

    /// 缓冲区太小
    BufferTooSmall { provided: usize, required: usize },

    /// 设备未打开
    DeviceNotOpen,

    /// 设备已关闭
    DeviceClosed,

    /// I/O错误
    IoError,

    /// 对齐错误（数据未对齐到块边界）
    AlignmentError { offset: u64, alignment: u32 },

    /// 设备忙
    DeviceBusy,

    /// 超时
    Timeout,

    /// 不支持的操作
    Unsupported,

    /// 设备只读
    ReadOnly,

    /// 空间不足
    NoSpace,

    /// 权限错误
    PermissionDenied,

    /// 设备损坏或数据损坏
    Corrupted,

    /// 校验和错误
    ChecksumError,

    /// 未知错误
    Unknown,
}


impl core::fmt::Display for BlockDevError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BlockDevError::InvalidInput =>{write!(f,"invalid input")}
            BlockDevError::ReadError => write!(f, "failed to read from block device"),
            BlockDevError::WriteError => write!(f, "failed to write to block device"),
            BlockDevError::BlockOutOfRange {
                block_id,
                max_blocks,
            } => {
                write!(f, "block id {block_id} out of range (max {max_blocks})")
            }
            BlockDevError::InvalidBlockSize { size, expected } => {
                write!(f, "invalid block size {size} (expected {expected})")
            }
            BlockDevError::BufferTooSmall { provided, required } => {
                write!(
                    f,
                    "buffer too small: provided {provided} bytes, required {required} bytes"
                )
            }
            BlockDevError::DeviceNotOpen => write!(f, "device not open"),
            BlockDevError::DeviceClosed => write!(f, "device already closed"),
            BlockDevError::IoError => write!(f, "I/O error"),
            BlockDevError::AlignmentError { offset, alignment } => {
                write!(
                    f,
                    "alignment error: offset {offset} is not aligned to {alignment}-byte boundary"
                )
            }
            BlockDevError::DeviceBusy => write!(f, "device is busy"),
            BlockDevError::Timeout => write!(f, "operation timed out"),
            BlockDevError::Unsupported => write!(f, "unsupported operation"),
            BlockDevError::ReadOnly => write!(f, "device is read-only"),
            BlockDevError::NoSpace => write!(f, "no space left on device"),
            BlockDevError::PermissionDenied => write!(f, "permission denied"),
            BlockDevError::Corrupted => write!(f, "device or data is corrupted"),
            BlockDevError::ChecksumError => write!(f, "checksum error"),
            BlockDevError::Unknown => write!(f, "unknown error"),
        }
    }
}
/// 块设备操作结果类型
pub type BlockDevResult<T> = Result<T, BlockDevError>;



/// Ext4文件系统错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RSEXT4Error {
    /// IO错误
    IoError,
    /// 魔数无效
    InvalidMagic,
    /// 超级块无效（如GDT超出预留空间）
    InvalidSuperblock,
    /// 文件系统有错误
    FilesystemHasErrors,
    /// 不支持的特性
    UnsupportedFeature,
    /// 已经挂载
    AlreadyMounted,
}

impl core::fmt::Display for RSEXT4Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RSEXT4Error::IoError => write!(f, "IO错误"),
            RSEXT4Error::InvalidMagic => write!(f, "魔数无效"),
            RSEXT4Error::InvalidSuperblock => write!(f, "超级块无效"),
            RSEXT4Error::FilesystemHasErrors => write!(f, "文件系统有错误"),
            RSEXT4Error::UnsupportedFeature => write!(f, "不支持的特性"),
            RSEXT4Error::AlreadyMounted => write!(f, "文件系统已挂载"),
        }
    }
}