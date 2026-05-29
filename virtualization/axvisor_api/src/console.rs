//! Console APIs for virtual console devices.

/// Console I/O interface exposed by the hypervisor host.
#[crate::api_def]
pub trait ConsoleIf {
    /// Writes raw bytes to the host console.
    fn write_bytes(bytes: &[u8]);

    /// Reads bytes from the host console into `bytes`.
    ///
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize;
}
