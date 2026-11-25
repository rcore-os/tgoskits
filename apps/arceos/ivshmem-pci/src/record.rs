//! Logical record interface shared by ivshmem message applications.
//!
//! Implementations own transport framing, session liveness, doorbells, and
//! event waits. Callers only start a record, stream its bytes with progress,
//! inspect the next record length, and wait for peer progress.

/// Progress made by one nonblocking record operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecordProgress {
    transferred: usize,
    progressed: bool,
    complete: bool,
}

impl RecordProgress {
    /// Creates a progress snapshot for one record operation.
    pub const fn new(transferred: usize, progressed: bool, complete: bool) -> Self {
        Self {
            transferred,
            progressed,
            complete,
        }
    }

    /// Returns the number of payload bytes accepted or produced.
    pub const fn transferred(self) -> usize {
        self.transferred
    }

    /// Returns whether this operation advanced either local or peer state.
    pub const fn progressed(self) -> bool {
        self.progressed
    }

    /// Returns whether this operation completed the current logical record.
    pub const fn is_complete(self) -> bool {
        self.complete
    }
}

/// One full-duplex logical-record endpoint.
///
/// Implementations preserve Message V1 record boundaries and notify the peer
/// whenever a write publishes slots or a read releases ring capacity.
pub trait MessageRecordEndpoint {
    /// Error reported by the device or message transport.
    type Error;

    /// Starts one outgoing logical record.
    fn start_record(&mut self, record_len: u64) -> Result<(), Self::Error>;

    /// Writes a prefix of the active record and reports backpressure.
    fn write_record(&mut self, input: &[u8]) -> Result<RecordProgress, Self::Error>;

    /// Returns the declared length of the current or next incoming record.
    fn peek_record_len(&mut self) -> Result<Option<u64>, Self::Error>;

    /// Reads a prefix of the current or next incoming record.
    fn read_record(&mut self, output: &mut [u8]) -> Result<RecordProgress, Self::Error>;

    /// Returns whether every published outbound slot has been consumed.
    ///
    /// Callers invoke this only after the last outgoing record is fully
    /// published. Successful completion makes the peer's published receive
    /// credit an observable delivery barrier without adding wire messages.
    fn outbound_drained(&mut self) -> Result<bool, Self::Error>;

    /// Waits for peer progress and returns whether a device event was observed.
    fn wait_for_progress(&mut self) -> Result<bool, Self::Error>;
}
