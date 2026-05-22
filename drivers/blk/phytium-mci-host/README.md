# phytium-mci-host

Portable `no_std` Phytium MCI/FSDIF host-controller backend for
`sdmmc-protocol`.

The crate owns register programming, command/response handling, FIFO and IDMAC
block transfers, clock timing selection, and IRQ event extraction. Platform code still
owns FDT/ACPI probe, MMIO mapping lifetime, IRQ registration, pad-controller
setup, and block-device registration.
