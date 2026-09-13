//! Host-side network backend capability boundary.

use crate::error::NetworkBackendError;

/// Transmit-side network backend: how guest TX frames leave the device model.
///
/// The first version only models guest -> host transmission here. Host -> guest
/// (RX) is driven explicitly by the VMM calling
/// [`receive_frame`](crate::VirtioMmioNetDevice::receive_frame), because TAP,
/// virtual switches and async runtimes all have different "frame arrived"
/// models and blocking inside a queue-notify handler is not portable.
///
/// Backends must not re-enter the device model from within [`transmit`](Self::transmit).
pub trait NetworkBackend: Send + Sync {
    /// Transmit a complete L2 frame (no `virtio_net_hdr`) to the host network.
    fn transmit(&self, frame: &[u8]) -> Result<(), NetworkBackendError>;

    /// Notifies the asynchronous backend that the guest kicked its RX queue.
    ///
    /// Backends that retained a frame because no guest RX buffer was available
    /// use this readiness edge to retry from deferred context. The default is a
    /// no-op for transmit-only backends.
    fn rx_queue_notified(&self) {}
}
