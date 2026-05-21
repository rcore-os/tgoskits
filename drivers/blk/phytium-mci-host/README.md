# phytium-mci-host

Portable `no_std` Phytium MCI/FSDIF host-controller backend for
`sdmmc-protocol`.

The crate owns register programming, command/response handling, FIFO data
transfer, clock timing selection, and IRQ event extraction. Platform code still
owns FDT/ACPI probe, MMIO mapping lifetime, IRQ registration, pad-controller
setup, and block-device registration.

DMA/IDMAC support is intentionally left out of the first portable cut. The
public block request API accepts `BlockTransferMode::Dma` so callers can fall
back to FIFO consistently, but DMA submissions currently return
`Error::UnsupportedCommand`.
