pub use ax_block::{BlockError, BlockResult};
use axfs_ng_vfs::VfsError;
pub(crate) use axfs_ng_vfs::{io_error_to_vfs_error, vfs_error_to_io_error};
use irq_framework::IrqError;
#[cfg(all(test, any(feature = "ext4", feature = "fat")))]
use rdif_block::{BlkError, RequestOp};

/// Adapt a block-runtime failure at the VFS implementation boundary.
pub(crate) fn block_error_to_vfs_error(error: BlockError) -> VfsError {
    match error {
        BlockError::InvalidRequest => VfsError::InvalidInput,
        BlockError::InvalidState | BlockError::RuntimeUnavailable => VfsError::BadState,
        BlockError::WouldBlock => VfsError::WouldBlock,
        BlockError::NoMemory => VfsError::NoMemory,
        BlockError::Unsupported => VfsError::Unsupported,
        BlockError::TimedOut => VfsError::TimedOut,
        BlockError::Io => VfsError::Io,
        BlockError::Device { source, .. } => block_error_to_vfs_error(source.into()),
        BlockError::ResourceBusy => VfsError::ResourceBusy,
        BlockError::NotFound => VfsError::NotFound,
        BlockError::Irq(error) => match error {
            IrqError::InvalidIrq | IrqError::InvalidCpu => VfsError::InvalidInput,
            IrqError::CpuOffline | IrqError::Unsupported => VfsError::Unsupported,
            IrqError::Busy | IrqError::InIrqContext => VfsError::ResourceBusy,
            IrqError::Timeout => VfsError::TimedOut,
            IrqError::NoMemory => VfsError::NoMemory,
            IrqError::NotFound => VfsError::NotFound,
            IrqError::Controller => VfsError::Io,
        },
    }
}

#[cfg(all(test, any(feature = "ext4", feature = "fat")))]
mod tests {
    use super::*;

    #[test]
    fn contextual_device_errors_keep_their_vfs_category() {
        let cases = [
            (BlkError::NotSupported, VfsError::Unsupported),
            (BlkError::Retry, VfsError::WouldBlock),
            (BlkError::NoMemory, VfsError::NoMemory),
            (BlkError::InvalidBlockIndex(7), VfsError::InvalidInput),
            (BlkError::InvalidRequest, VfsError::InvalidInput),
            (BlkError::TimedOut, VfsError::TimedOut),
            (BlkError::Io, VfsError::Io),
            (BlkError::Other("device failure"), VfsError::Io),
        ];

        for (source, expected) in cases {
            let error = BlockError::Device {
                stage: "submit",
                operation: RequestOp::Read,
                lba: 11,
                source,
            };
            assert!(core::error::Error::source(&error).is_some());
            assert_eq!(block_error_to_vfs_error(error), expected);
        }
    }
}
