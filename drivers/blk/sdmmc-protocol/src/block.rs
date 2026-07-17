//! Block request state shared by SD/MMC host controller backends.
//!
//! The protocol crate intentionally does not know about a block runtime or any
//! executor. These types describe the portable queue contract that host
//! drivers expose upward: submit one block transfer, advance it from stable IRQ
//! snapshots, and keep the concrete FIFO/DMA engine visible. Eventless waits
//! belong to the explicit initialization/recovery state machines, not runtime
//! request completion.

use core::num::NonZeroUsize;

/// Stable identifier returned by a host block queue after submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockRequestId(usize);

impl BlockRequestId {
    pub const fn new(id: usize) -> Self {
        Self(id)
    }
}

impl From<BlockRequestId> for usize {
    fn from(value: BlockRequestId) -> Self {
        value.0
    }
}

/// Data engine used by an in-flight block request.
///
/// Marked `#[non_exhaustive]`: new engines (e.g. SDMA, ADMA3) may be added
/// before 1.0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlockTransferMode {
    /// Controller FIFO/data-port engine.
    Fifo,
    /// Host-controller DMA engine (SDHCI ADMA2, DW_mshc IDMAC, etc.).
    Dma,
}

/// Buffer and address constraints exposed by a host block queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockBufferConfig {
    /// Logical block size accepted by the queue.
    pub block_size: NonZeroUsize,
    /// Required CPU-buffer alignment in bytes.
    pub align: usize,
    /// Device-visible DMA address mask, when the queue uses DMA.
    pub dma_mask: Option<u64>,
}

impl BlockBufferConfig {
    pub const fn new(block_size: NonZeroUsize, align: usize, dma_mask: Option<u64>) -> Self {
        Self {
            block_size,
            align,
            dma_mask,
        }
    }
}

/// Direction of a block request.
///
/// Marked `#[non_exhaustive]` for forward compatibility (future variants like
/// `Erase` may join the queue contract).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlockTransferDirection {
    Read,
    Write,
}

/// Observable state of one host block-transfer state machine.
///
/// Marked `#[non_exhaustive]`: queue-level intermediate states (e.g. tuning,
/// drain) may be added before 1.0; downstream match sites must keep a
/// `_ => ...` arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlockTransferState {
    #[default]
    Idle,
    Submitted {
        id: BlockRequestId,
        mode: BlockTransferMode,
        direction: BlockTransferDirection,
    },
    Complete {
        id: BlockRequestId,
        mode: BlockTransferMode,
        direction: BlockTransferDirection,
    },
    Failed {
        id: BlockRequestId,
        mode: BlockTransferMode,
        direction: BlockTransferDirection,
    },
}

impl BlockTransferState {
    pub const fn id(self) -> Option<BlockRequestId> {
        match self {
            Self::Idle => None,
            Self::Submitted { id, .. } | Self::Complete { id, .. } | Self::Failed { id, .. } => {
                Some(id)
            }
        }
    }

    pub const fn mode(self) -> Option<BlockTransferMode> {
        match self {
            Self::Idle => None,
            Self::Submitted { mode, .. }
            | Self::Complete { mode, .. }
            | Self::Failed { mode, .. } => Some(mode),
        }
    }

    pub const fn direction(self) -> Option<BlockTransferDirection> {
        match self {
            Self::Idle => None,
            Self::Submitted { direction, .. }
            | Self::Complete { direction, .. }
            | Self::Failed { direction, .. } => Some(direction),
        }
    }
}

/// Result of advancing a submitted transfer without blocking.
///
/// Marked `#[non_exhaustive]`: intermediate states such as `Aborted` or
/// per-block progress may be added before 1.0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlockPoll {
    Pending,
    Complete,
}

/// Direction of a generic SD/MMC data command.
///
/// Marked `#[non_exhaustive]` for forward compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataCommandDirection {
    Read,
    Write,
}

/// Observable state of one protocol data command.
///
/// Marked `#[non_exhaustive]` for forward compatibility.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataCommandState {
    #[default]
    Idle,
    Submitted {
        direction: DataCommandDirection,
        cmd_index: u8,
        block_size: u32,
        block_count: u32,
    },
}

/// Result of advancing a generic data command without blocking.
///
/// Marked `#[non_exhaustive]` for forward compatibility.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum DataCommandPoll {
    Pending,
    Complete(crate::response::Response),
}

/// Result of advancing a submitted command without blocking.
///
/// Marked `#[non_exhaustive]` for forward compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommandPoll {
    Pending,
    Complete,
}

/// Result of advancing a submitted command and harvesting its response when
/// available.
///
/// Marked `#[non_exhaustive]` for forward compatibility.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum CommandResponsePoll {
    Pending,
    Complete(crate::response::Response),
}

/// Generic result of advancing an operation without blocking.
///
/// Marked `#[non_exhaustive]` for forward compatibility.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum OperationPoll<T> {
    Pending,
    Complete(T),
}

impl From<CommandResponsePoll> for OperationPoll<crate::response::Response> {
    fn from(value: CommandResponsePoll) -> Self {
        match value {
            CommandResponsePoll::Pending => Self::Pending,
            CommandResponsePoll::Complete(response) => Self::Complete(response),
        }
    }
}

impl From<DataCommandPoll> for OperationPoll<crate::response::Response> {
    fn from(value: DataCommandPoll) -> Self {
        match value {
            DataCommandPoll::Pending => Self::Pending,
            DataCommandPoll::Complete(response) => Self::Complete(response),
        }
    }
}

impl From<BlockPoll> for OperationPoll<()> {
    fn from(value: BlockPoll) -> Self {
        match value {
            BlockPoll::Pending => Self::Pending,
            BlockPoll::Complete => Self::Complete(()),
        }
    }
}
