# phytium-mci-host

Portable `no_std` Phytium MCI/FSDIF host-controller backend for
`sdmmc-protocol`.

The crate owns register programming, command/response handling, FIFO and IDMAC
block transfers, clock timing selection, and IRQ event extraction. Platform code still
owns FDT/ACPI probe, MMIO mapping lifetime, IRQ registration, pad-controller
setup, and block-device registration.

`sdmmc_protocol::sdio::SdioSdmmc::new_host2_timed` drives reset, power, voltage,
bus-width, clock, and SD/MMC commands through the native initialization state
machine. That initialization path may use FIFO and is explicitly rescheduled by
the caller.

Normal RDIF 0.12 block I/O requires an installed `dma_api::DeviceDma`, a bound
and enabled IRQ source, and the owned IDMAC queue. The first initialization bus
operation is rejected until completion IRQ delivery is enabled. FIFO-only
configuration is initialization only and cannot publish a runtime queue.

In runtime mode the IRQ endpoint alone reads and acknowledges interrupt status;
task context advances requests only from stable event snapshots and never polls
hardware for completion. IDMAC descriptor completion and controller data
transfer completion are independent conditions. R1b commands additionally wait
for the controller's IRQ-reported busy release before returning the response.
Runtime aborts return `Busy` until the controller lifecycle has masked and
synchronized both controller and IDMAC IRQ delivery and proved DMA quiescence;
no worker-side reset is used to reclaim an in-flight buffer.
