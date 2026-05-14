use alloc::vec::Vec;

pub use usb_if::endpoint::TransferKind;

#[cfg_attr(umod, derive(Clone))]
pub struct Transfer {
    pub kind: TransferKind,
    pub direction: usb_if::transfer::Direction,
    #[cfg(kmod)]
    pub mapping: Option<dma_api::SArrayPtr<u8>>,
    #[cfg(umod)]
    pub buffer: Option<(std::ptr::NonNull<u8>, usize)>,
    pub transfer_len: usize,
    pub iso_packet_actual_lengths: Vec<usize>,
}
