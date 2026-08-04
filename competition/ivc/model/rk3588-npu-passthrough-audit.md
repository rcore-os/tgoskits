# RK3588 NPU passthrough audit

> Audit date: 2026-08-04
>
> Board: Xunlong Orange Pi 5 Plus, RK3588
>
> Linux reference: `6.1.43-rockchip-rk3588`

## Confirmed physical resources

The values below were read under a board lease from the running Linux device
tree at `/boot/dtb-6.1.43-rockchip-rk3588/rockchip/rk3588-orangepi-5-plus.dtb`.
That DTB has SHA-256
`afefb78e51740537dfb44d26f9f58f5d84be7243332e673d4749c55f17f643bb`.

| Resource | Physical range or interrupt | Live-DTB identity |
| --- | --- | --- |
| NPU core 0 | `0xfdab0000 + 0x10000` | `/npu@fdab0000`, first `reg` tuple |
| NPU core 1 | `0xfdac0000 + 0x10000` | `/npu@fdab0000`, second `reg` tuple |
| NPU core 2 | `0xfdad0000 + 0x10000` | `/npu@fdab0000`, third `reg` tuple |
| NPU interrupts | GIC SPI 110, 111, 112; Linux IRQ 142, 143, 144 | level-high |
| NPU IOMMU | `0xfdab9000`, `0xfdaba000`, `0xfdaca000`, `0xfdada000`, each `0x100` | `/iommu@fdab9000` |
| PMU | `0xfd8d8000 + 0x400` | `/power-management@fd8d8000` |
| CRU | `0xfd7c0000 + 0x5c000` | `/clock-controller@fd7c0000` |

Linux exposes `/dev/dri/card1` through the built-in RKNPU 0.9.6 driver. The
three NPU interrupts are shared by `fdab0000.npu` and `fdab9000.iommu`. The
observed devfreq range is 300 MHz to 1 GHz.

## Current competition gap

The competition StarryOS guest configuration currently has no passthrough
device or address, and its synthetic DTB has no
`rockchip,rk3588-rknpu` node. Its kernel build also lacks the `rknpu` feature.
Therefore the existing bare-metal RKNN example is reusable implementation
evidence, but not AxVisor guest execution evidence.

The repository's current StarryOS RKNPU driver maps the three core register
ranges, allocates physically contiguous DMA below 4 GiB, and polls completion
in the submit path. It does not use the Linux Rockchip IOMMU driver. The first
guest spike must therefore omit `iommus` and OPP/regulator properties rather
than presenting dependencies that StarryOS cannot own or service.

## Required ownership gate

PMU and CRU registers control the whole SoC and must not be passed wholesale to
an untrusted guest. The target boundary is:

```text
AxVisor host board glue
  owns PMU/CRU sequencing and freezes the selected NPU clock
  hands off an enabled, reset-deasserted NPU
        |
        v
StarryOS guest
  exclusively maps the three NPU core ranges
  uses IOMMU-bypass contiguous DMA
  runs RKNN Runtime through /dev/dri/card1
```

Before the guest is started, the host must prove that it enabled the NPU parent
and core power domains, configured clocks, deasserted resets, and does not probe
or submit to the NPU afterward. The guest DTB must contain only the resources it
actually owns. AxVisor must reject overlapping ownership by another guest.

The initial compatibility spike may use polling and no passthrough IRQ because
that matches the current Starry driver. IRQ routing can be added only after a
driver path consumes it and has a deterministic contract test. A successful
`rknn_init` alone is insufficient: acceptance requires submit counters,
`RKNN_QUERY_PERF_RUN`, correct golden-vector output, and a clean post-run board
recovery.
