//! Block request state shared by SD/MMC host controller backends.
//!
//! The protocol crate intentionally does not know about `rd-block` or any
//! executor. These types describe the portable queue contract that host
//! drivers expose upward: submit one block transfer, advance it by polling or
//! IRQ wakeups, and keep the concrete FIFO/DMA engine visible.

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockTransferMode {
    /// Controller FIFO/data-port engine.
    Fifo,
    /// Host-controller DMA engine (SDHCI ADMA2, DW_mshc IDMAC, etc.).
    Dma,
}

/// Direction of a block request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockTransferDirection {
    Read,
    Write,
}

/// Observable state of one host block-transfer state machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockPoll {
    Pending,
    Complete,
}

/// Direction of a generic SD/MMC data command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataCommandDirection {
    Read,
    Write,
}

/// Observable state of one protocol data command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug)]
pub enum DataCommandPoll {
    Pending,
    Complete(crate::response::Response),
}

/// Result of advancing a submitted command without blocking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandPoll {
    Pending,
    Complete,
}

/// Result of advancing a submitted command and harvesting its response when
/// available.
#[derive(Clone, Copy, Debug)]
pub enum CommandResponsePoll {
    Pending,
    Complete(crate::response::Response),
}

/// Generic result of advancing an operation without blocking.
#[derive(Clone, Copy, Debug)]
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

/// Drive a non-blocking operation to completion with a caller-provided
/// pending strategy.
///
/// This is the synchronous executor for the submit/poll state machines. IRQ
/// and async runtimes should not call it; they should call the same `poll`
/// function once and arrange a wakeup when it returns [`OperationPoll::Pending`].
pub fn block_on_poll<T, E, F>(mut poll: F, mut on_pending: impl FnMut()) -> Result<T, E>
where
    F: FnMut() -> Result<OperationPoll<T>, E>,
{
    loop {
        match poll()? {
            OperationPoll::Pending => on_pending(),
            OperationPoll::Complete(value) => return Ok(value),
        }
    }
}
