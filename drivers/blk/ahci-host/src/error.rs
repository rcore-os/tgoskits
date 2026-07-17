/// AHCI discovery or resource-construction failure.
#[derive(Debug, thiserror::Error)]
pub enum AhciError {
    #[error("AHCI MMIO mapping failed: {0}")]
    Mmio(#[from] mmio_api::MapError),
    #[error("AHCI MMIO aperture is too small: required {required:#x}, actual {actual:#x}")]
    MmioApertureTooSmall { required: usize, actual: usize },
    #[error("invalid AHCI configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("AHCI port {port} has no available ATA device view")]
    PortUnavailable { port: usize },
    #[error("AHCI DMA resource allocation failed: {0}")]
    Dma(#[from] dma_api::DmaError),
}
