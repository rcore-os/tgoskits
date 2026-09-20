use axvirtio_common::VirtioResult;

/// Trait for block device backends
pub trait BlockBackend: Send + Sync {
    /// Whether retrying the current deferred request can make progress.
    /// Returning false lets the transport skip descriptor reads, allocation,
    /// and data copies. This query must not consume the completion. Backends
    /// returning false must notify their runtime when progress becomes possible,
    /// including after cancellation drains. The default retains eager retries.
    fn pending_request_ready(&self) -> bool {
        true
    }

    /// Abandons the current deferred request when the transport stops retrying it.
    /// Must be nonblocking and idempotent. Running I/O may drain, but its result
    /// must never be consumed by the next request, even for an identical range.
    /// Called on every terminal request path, including successful completion.
    fn cancel_pending_request(&self) {}

    /// Invalidates asynchronous results after a transport reset. Must not block.
    /// Already executing storage I/O may drain, but must not complete a new request.
    fn reset(&self) {
        self.cancel_pending_request();
    }

    /// Returns whether queue processing must be deferred to a pollable runtime
    /// context because backend operations can block.
    fn requires_deferred_processing(&self) -> bool {
        false
    }

    /// Read data from the device
    ///
    /// # Arguments
    /// * `sector` - Starting sector number
    /// * `buffer` - Buffer to read data into
    ///
    /// # Returns
    /// Number of bytes read on success
    fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize>;

    /// Write data to the device
    ///
    /// # Arguments
    /// * `sector` - Starting sector number
    /// * `buffer` - Buffer containing data to write
    ///
    /// # Returns
    /// Number of bytes written on success
    fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize>;

    /// Flush any pending writes to the device
    fn flush(&self) -> VirtioResult<()>;
}
