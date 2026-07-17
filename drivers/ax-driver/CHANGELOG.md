# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Re-export the bounded runtime serial emergency-write capability for OS glue.
- Require every VirtIO transport crossing discovery, registration, or RDIF
  ownership boundaries to implement `Send`; driver wrappers can no longer make
  an arbitrary upstream `Transport` cross CPUs through an unsafe blanket.
- Pass the move-only initialized-card capability through every SD/MMC staged
  probe so platform glue cannot publish a queue before card identification.
- Preserve controller bundles through driver-core registration instead of
  collapsing every controller to one device interface, and register AHCI HBAs
  once while exposing each identified port as an independent logical disk.
- Defer SD/MMC card initialization to the runtime controller-activation state
  machine, and remove platform-side busy loops and synchronous phase-probe I/O.
- Stage Rockchip regulator, clock, and phase setup behind the initialization
  IRQ/worker binding; power-settle delays now use an absolute runtime deadline.
- Fail CV181x and StarFive FIFO-only block probes before touching hardware;
  these backends remain unpublished until they provide owned IRQ-only normal
  I/O queues.
- Migrate virtio-blk to IRQ-driven rdif-block 0.12 owned requests and restore
  exact DMA-buffer ownership on completion, rejection, and shutdown.
- Preallocate virtio-blk's single descriptor request/response storage during
  discovery so staged submit and IRQ service perform no per-request allocation
  or release, while unquiesced descriptors retain the same bounded quarantine.
- Use the blk-mq direct-dispatch fast path for virtio-blk behind hctx queue
  ownership, while contended transport acknowledgements become a typed,
  coalesced worker continuation before the used ring is inspected.
- Reject VirtIO controller initialization until its IRQ endpoint has been
  transferred and enabled, and mask device notifications before publishing the
  software-disabled IRQ state so the final shared or remote edge can still be
  acknowledged.
- Route contended VirtIO initialization acknowledgements through the same
  typed, bounded task-side continuation used by normal queue service.
- Reset a VirtIO controller and wait for status-zero acknowledgement before an
  initialization error becomes terminal; if reset cannot be proven, retain its
  bounded queue/request DMA storage in a fail-closed quarantine.
- Keep virtio-blk `EVENT_IDX` unnegotiated until the public queue API can
  suppress indexed used notifications, so device-side IRQ masking remains a
  real reset precondition rather than a no-op.
- Split virtio-blk discovery from reset, feature negotiation, stable capacity
  capture, queue installation, and DRIVER_OK publication. The bounded init FSM
  starts only after the runtime binds its IRQ action; capacity and queues stay
  unpublished until Ready.
- Build the block path from the public virtio-drivers `Transport` and
  `VirtQueue` APIs to avoid the eager, internally retrying `VirtIOBlk::new`
  constructor. Recovery now waits for an acknowledged device-status reset,
  discards the old queue only after it is no longer live, and reuses the staged
  feature/configuration/queue initializer for host recovery and guest return.
- Capture every VirtIO PCI memory BAR before transport construction mutates the
  endpoint, preserving exact host-controller identity for passthrough handoff.
- Preserve staged controller initialization through platform IRQ-lease
  wrappers, and order activation as OS action, transport/vector lease, then
  device source with rollback when device unmasking fails.
- Make IRQ-binding transitions return typed failures. MSI-X multi-vector enable
  is transactional, disable attempts every table/provider operation, and a
  failed binding can no longer be logged and published as an active block
  runtime. Teardown masks the device source before withdrawing its binding;
  failed masking keeps the binding live, while an unrecoverable MSI-X lease
  drop retains its vector token and table mapping.
- Register NVMe as a command-free discovery object; reset, Identify, queue
  creation, and capacity publication now run only through the runtime-bound
  initialization IRQ action. Any post-enable initialization failure disables
  the controller and waits for RDY=0 before becoming terminal; a disable
  timeout remains in shutdown-lifetime quarantine with DMA storage retained.
- Retain each block controller's stable `rdrive` device ID, firmware or PCI
  locator, and validated host MMIO/BAR ranges so passthrough handoff can select
  the exact device without inferring identity from an IRQ number.
- Reject a block controller before registry publication when either its normal
  queue endpoint or initialization state machine declares an IRQ source that
  firmware did not bind.
- Preserve every FDT interrupt specifier by its logical source index instead of
  silently retaining only source zero, allowing multi-source controller
  activation to validate the complete firmware binding.
- Retain a move-only PCI INTx endpoint-gate lease across AHCI, NVMe, and
  virtio-blk discovery; INTx remains masked until the runtime owns the IRQ
  action and is masked again during teardown.
- Allow NVMe to fall back from MSI-X to INTx only when MSI-X is unsupported;
  a programming or rollback failure now aborts probe with the endpoint kept
  masked instead of activating a second interrupt mode on unproven state.
- Retain the NVMe PCI endpoint inside its MSI-X lease through shutdown; if
  vector disable cannot be proven, quarantine the endpoint with the vector
  allocation and table mapping instead of releasing partial ownership.
- Move StarFive clock, reset, and regulator activation behind the bound IRQ
  action while retaining those resource capabilities in the staged controller
  through initialization, recovery, and ownership handoff.

## [0.12.0](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.11.3...ax-driver-v0.12.0) - 2026-07-10

### Added

- *(crab-usb)* add SG2002 DWC2 host axtest ([#1496](https://github.com/rcore-os/tgoskits/pull/1496))
- *(msi)* add hierarchical MSI-X irq domains ([#1526](https://github.com/rcore-os/tgoskits/pull/1526))

## [0.11.3](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.11.2...ax-driver-v0.11.3) - 2026-07-08

### Other

- updated the following local packages: axklib, ax-alloc

## [0.11.2](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.11.1...ax-driver-v0.11.2) - 2026-07-08

### Other

- updated the following local packages: ax-alloc, axklib

## [0.11.1](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.11.0...ax-driver-v0.11.1) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, dma-api, rd-net, aic8800, rdrive, ax-alloc, axklib, arm-scmi-rs, crab-usb, rdif-block, sdmmc-protocol, sdhci-host, cv181x-sdhci, dwmmc-host, eth-intel, nvme-driver, phytium-mci-host, ramdisk, realtek-rtl8125, rockchip-jpeg, rockchip-npu, rockchip-rga, rockchip-soc, starfive-jh7110-dwmmc

## [0.11.0](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.10.0...ax-driver-v0.11.0) - 2026-07-07

### Added

- *(rdrive)* apply assigned clocks before FDT probe ([#1527](https://github.com/rcore-os/tgoskits/pull/1527))
- *(starfive-jh7110-dwmmc)* add IRQ-driven host ([#1524](https://github.com/rcore-os/tgoskits/pull/1524))
- *(msi)* add aarch64 MSI-X registration ([#1522](https://github.com/rcore-os/tgoskits/pull/1522))
- *(rdrive)* add FDT power-domain probing ([#1515](https://github.com/rcore-os/tgoskits/pull/1515))
- *(starry)* support rtnetlink IPv4 configuration ([#1497](https://github.com/rcore-os/tgoskits/pull/1497))
- *(cv181x-sdhci)* add SG2002 SD driver ([#1482](https://github.com/rcore-os/tgoskits/pull/1482))
- *(crab-usb)* add RK3588 EHCI USB2 host ([#1481](https://github.com/rcore-os/tgoskits/pull/1481))
- *(starry-kernel)* add RK3588 PWM sysfs support ([#1468](https://github.com/rcore-os/tgoskits/pull/1468))

### Fixed

- *(block)* drive virtio-blk completions by IRQ ([#1512](https://github.com/rcore-os/tgoskits/pull/1512))
- *(starry)* apply termios serial format and handle tty drain ioctls ([#1484](https://github.com/rcore-os/tgoskits/pull/1484))

### Other

- *(drivers)* split Rockchip reset capability ([#1509](https://github.com/rcore-os/tgoskits/pull/1509))
- *(platforms)* move someboot and somehal-macros and add documents ([#1485](https://github.com/rcore-os/tgoskits/pull/1485))
- *(sdmmc-protocol)* split SDIO and RDIF capability modules ([#1486](https://github.com/rcore-os/tgoskits/pull/1486))
- remove static platform and axconfig generation, make dynamic platform the only path ([#1478](https://github.com/rcore-os/tgoskits/pull/1478))

## [0.10.0](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.9.0...ax-driver-v0.10.0) - 2026-07-02

### Added

- *(rockchip-jpeg)* add RK3588 hardware JPEG decoder (VDPU720) with MPP /dev/mpp_service ([#1456](https://github.com/rcore-os/tgoskits/pull/1456))
- *(rdif-pinctrl)* add FDT pinctrl apply support ([#1433](https://github.com/rcore-os/tgoskits/pull/1433))

### Fixed

- *(ci)* prevent Starry qemu hangs in IRQ paths ([#1431](https://github.com/rcore-os/tgoskits/pull/1431))
- *(irq)* close domain runtime review gaps

### Other

- *(ax-driver)* remove static platform compatibility ([#1463](https://github.com/rcore-os/tgoskits/pull/1463))
- *(rdrive)* apply default FDT pinctrl before probe ([#1458](https://github.com/rcore-os/tgoskits/pull/1458))
- *(irq-framework)* require boxed IRQ callbacks ([#1452](https://github.com/rcore-os/tgoskits/pull/1452))
- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))
- *(net)* split IRQ handlers from NIC queues ([#1435](https://github.com/rcore-os/tgoskits/pull/1435))
- *(ax-runtime)* resolve device IRQ bindings to IrqId

## [0.9.0](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.8.2...ax-driver-v0.9.0) - 2026-06-27

### Added

- *(rdif-block)* add owned DMA queue primitives
- *(ax-driver)* add VisionFive2 dynamic rtc and mmc ([#1353](https://github.com/rcore-os/tgoskits/pull/1353))

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))
- *(ax-driver)* serialize virtio-net queue access ([#1392](https://github.com/rcore-os/tgoskits/pull/1392))
- *(rknpu)* honor GEM cache flags for mmap ([#1364](https://github.com/rcore-os/tgoskits/pull/1364))

### Other

- *(ax-driver)* use native SDMMC RDIF devices
- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.8.2](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.8.1...ax-driver-v0.8.2) - 2026-06-23

### Other

- updated the following local packages: rd-net, aic8800, ax-kspin, dma-api, ax-alloc, axklib, some-serial, crab-usb, dwmmc-host, eth-intel, rdif-block, nvme-driver, phytium-mci-host, ramdisk, realtek-rtl8125, rockchip-npu, rockchip-rga, rockchip-soc, sdhci-host

## [0.8.1](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.8.0...ax-driver-v0.8.1) - 2026-06-22

### Added

- *(starry)* add Wayland app case ([#1160](https://github.com/rcore-os/tgoskits/pull/1160))
- *(rockchip-rga)* add dry-run command buffer ([#1248](https://github.com/rcore-os/tgoskits/pull/1248))
- AIC8800 Wi-Fi SoftAP for SG2002 (LicheeRV Nano) ([#1185](https://github.com/rcore-os/tgoskits/pull/1185))

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.7.1...ax-driver-v0.8.0) - 2026-06-12

### Added

- *(ax-driver)* add dynamic platform rtc support ([#1242](https://github.com/rcore-os/tgoskits/pull/1242))

### Fixed

- *(somehal)* route LoongArch ACPI GSIs through PCH-PIC

### Other

- *(irq)* carry ACPI IRQ routing metadata
- *(ax-driver)* normalize FDT PCI IRQ source resolution
- *(ax-driver)* register devices with binding info
- *(rdrive)* carry probe context and PCI INTx routes

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.7.0...ax-driver-v0.7.1) - 2026-06-11

### Fixed

- *(kernel)* harden early allocation and virtio PCI setup

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.6.1...ax-driver-v0.7.0) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))
- *(somehal)* register x86 ACPI IOAPIC through rdrive ([#1155](https://github.com/rcore-os/tgoskits/pull/1155))

### Fixed

- *(ci)* switch x86_64 defaults to dynamic platform ([#1024](https://github.com/rcore-os/tgoskits/pull/1024))

### Other

- *(ax-driver)* remove redundant mmio cfg gate ([#1100](https://github.com/rcore-os/tgoskits/pull/1100))

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.6.0...ax-driver-v0.6.1) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))
- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(starryos)* add QEMU K230 boot support ([#1046](https://github.com/rcore-os/tgoskits/pull/1046))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))
- *(some-serial)* add Rockchip FIQ debugger UART ([#980](https://github.com/rcore-os/tgoskits/pull/980))

### Other

- *(platform)* migrate riscv64 qemu to dynamic platform ([#1085](https://github.com/rcore-os/tgoskits/pull/1085))
- *(platform)* remove static aarch64 platforms ([#1074](https://github.com/rcore-os/tgoskits/pull/1074))
- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.5.14...ax-driver-v0.6.0) - 2026-05-19

### Fixed

- *(starry)* weston bringup fixes + IRQ wakers + AF_UNIX cmsg byte marks ([#509](https://github.com/rcore-os/tgoskits/pull/509))

### Other

- Refactor Clippy integration and enhance package handling ([#738](https://github.com/rcore-os/tgoskits/pull/738))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.5.13...ax-driver-v0.5.14) - 2026-05-15

### Other

- updated the following local packages: ax-driver-input, ax-driver-virtio, ax-alloc, ax-config, axplat-dyn, ax-hal, ax-driver-net, ax-dma

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.5.11...ax-driver-v0.5.12) - 2026-04-27

### Other

- updated the following local packages: ax-alloc
