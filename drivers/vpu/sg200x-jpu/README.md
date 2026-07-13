# sg200x-jpu

`sg200x-jpu` is a `no_std` driver for the JPEG Processing Unit found in
SG200x SoCs. It supports baseline JPEG decoding at full, half, quarter, and
eighth resolution and reports the exact planar output layout.

The register programming and JPEG parser were extracted from
[`yfblock/sg200x-bsp`](https://github.com/yfblock/sg200x-bsp) version 0.7.1.
This crate adds checked layouts, explicit DMA ownership, bounded hardware
polling, and scaled decode. The original MIT copyright notice is retained in
`LICENSE-MIT`.

The crate does not choose physical MMIO addresses and does not perform
platform-specific cache maintenance. Callers provide mapped bases through
`JpuMmio` and a `dma_api::DeviceDma` whose backend owns allocation, address
translation, and cache synchronization.
