# phytium-mci-host

Portable `no_std` Phytium MCI/FSDIF host-controller backend for
`sdmmc-protocol`.

The crate owns command/response state, a controller-lifetime 4 KiB IDMAC
descriptor ring, clock timing selection, and minimal IRQ acknowledgement.
Platform glue owns discovery, MMIO lifetime, IRQ registration, pad/clock/reset
setup, and the `dma-api` domain.

Production block I/O is owned-DMA and IDMAC-only. `configure_dma` currently
keeps the hardware-validated 32-bit DMA mask. Command/data progression requires
an acknowledged IRQ; bounded register retries are used only for reset and
clock-stable states. There is no FIFO fallback, DMA-capability cloning, or
completion polling.

If recovery cannot prove that DMA is quiescent, the in-flight ownership is
quarantined instead of being returned unsafely.

```bash
cargo test -p phytium-mci-host
cargo xtask clippy --package phytium-mci-host
```
