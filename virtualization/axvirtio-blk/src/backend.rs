use axvirtio_common::VirtioResult;

/// Trait for block device backends
pub trait BlockBackend: Send + Sync {
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
