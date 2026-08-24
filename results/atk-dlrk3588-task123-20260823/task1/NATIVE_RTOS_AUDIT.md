# Native bare-metal RTOS feasibility audit

Status: BLOCKED for this exact ATK-DLRK3588 board. This status is not counted as
a physical native-baseline pass.

## Audited material

The RT-Thread source snapshot is commit
`6ea682795bdbac59d3700b21e159ccaa3f7632cb`. Its `bsp/rockchip/` top level
contains `common`, `dm`, `rk2108`, `rk3300`, `rk3500`, and `tools`; it has no
RK3588 or ATK-DLRK3588 board directory.

There are reusable RK3588-aware drivers, including clock, thermal, ADC and a
GIC ITS erratum entry. Those drivers do not constitute a bootable BSP. Missing
board integration includes the startup/EL transition, linker and load layout,
ATK UART selection, physical GIC description, timer/PSCI/SMP wiring, board DT,
and the BL31/vendor-U-Boot handoff contract.

The tested RT-Thread periodic image is explicitly built from
`bsp/qemu-virt64-aarch64`. Its manifest records memory base `0xA0000000`, entry
`0xA0080000`, and the guest configuration supplies a virtual PL011 at
`0x09000000` plus a virtual interrupt environment. It is therefore a valid
AxVisor guest payload, not a native RK3588 boot image. The archived README,
`rtconfig.h`, QEMU DTS and manifest make this distinction inspectable.

The available official Zephyr TOML is likewise a guest-image placeholder for
Orange Pi 5 Plus; it is neither an ATK-DLRK3588 native board source tree nor a
direct-boot artifact.

## Why no image was staged as a native baseline

Loading the QEMU/virtualized RT-Thread binary directly would jump into an image
linked for virtual memory and virtual devices. A hang or exception would test
an invalid binary/board contract, not native RTOS latency. Creating the missing
BSP is a separate board-porting project and cannot be represented as a baseline
measurement.

The blocker can be removed only by adding or obtaining a reviewed RK3588 BSP
for this board, proving UART/GIC/timer/PSCI and memory initialization, defining
a RAM-only U-Boot load/entry contract, and then porting the same periodic probe.

