# phytium-mci-host

Portable `no_std` Phytium MCI/FSDIF host-controller backend for
`sdmmc-protocol`.

The crate owns register programming, command/response handling, FIFO and IDMAC
block transfers, clock timing selection, and IRQ event extraction. Platform code still
owns FDT/ACPI probe, MMIO mapping lifetime, IRQ registration, pad-controller
setup, and block-device registration.

`sdmmc_protocol::sdio::SdioSdmmc::new_host2` drives reset, power, voltage,
bus-width, clock, and SD/MMC commands through the native `sdio-host2`
submit/poll model. Install optional DMA capability with `PhytiumMci::set_dma`
before handing the host to the protocol layer; block data transactions then try
IDMAC first for 512-byte CMD17/CMD18/CMD24/CMD25 requests and fall back to FIFO
when DMA is unavailable or not applicable.
