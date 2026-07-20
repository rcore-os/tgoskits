# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Replace the legacy `Vgic`/GICv2 API with a GICv3-only per-VM controller,
  validated configuration and typed INTID/affinity/ITS identifiers.
- Move MMIO, host GIC discovery, guest memory, timer and scheduler integration
  behind checked backend capabilities owned by the VMM.
- Save and restore complete virtual CPU-interface state, retain software
  pending interrupts when LRs are full, and drive maintenance refill without
  panicking.
- Implement a bounded software ITS command queue and explicit physical
  SPI/MSI ownership with cleanup on controller drop.
- Keep physical SPIs masked until their guest enables both Distributor gates,
  and mask them again when either gate is disabled.
- Filter explicit-ownership GICD accesses per VM while keeping every GICR,
  SGI, and PPI entirely VM-local. Mixed Distributor writes never modify
  host-owned SPI state.
- Defer physical SPI handoff until activation, then restore the saved host
  group, priority, trigger, route, pending, active, and enable state on release.
- Report physical `GICD_TYPER` capacity and make LPI registers RAZ/WI unless an
  emulated guest ITS is present.
- Replace the controller-wide emulated/passthrough mode with an SPI ownership
  policy and per-endpoint software or physical backing. One GIC can now deliver
  virtual-device IRQs and owned physical IRQs through the same CPU interface.
- Rebuild the LR working set when active entries overflow, preserve software or
  physical backing outside LRs, drive NPIE/LRENPIE/UIE/TDIR maintenance, and
  reconcile EOImode 0 EOIcount or trapped EOImode 1 DIR without starving newer
  pending interrupts.
- Make trapped DIR harvest the live ICH state before deactivation, so a hardware
  Pending-to-Active LR transition cannot be missed by a stale VM-local snapshot.
- Fall back from the optional dedicated `TDIR` trap to the architectural common
  CPU-interface trap, including checked CTLR, PMR, and RPR emulation, on GICv3
  implementations that report `ICH_VTR_EL2.TDS=0`.
- Keep an owned physical SPI armed from VM-local Distributor state even while
  its target vCPU is not loaded, so host scheduling cannot disconnect a guest
  device interrupt source.

### Removed

- Remove GICv2 support, global host callbacks and global ITS/LPI state, the
  embedded virtual timer, GPA=HPA assumptions and manual inject functions.

## [0.5.3](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.5.2...arm_vgic-v0.5.3) - 2026-07-08

### Other

- updated the following local packages: ax-kspin

## [0.5.2](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.5.1...arm_vgic-v0.5.2) - 2026-07-07

### Other

- update Cargo.toml dependencies

## [0.5.1](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.5.0...arm_vgic-v0.5.1) - 2026-07-02

### Other

- updated the following local packages: ax-kspin, ax-errno, axvm-types, axdevice_base

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.4.14...arm_vgic-v0.5.0) - 2026-06-27

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))

### Other

- *(axdevice)* unify Device model with indexed dispatch and conflict detect ([#1335](https://github.com/rcore-os/tgoskits/pull/1335))

## [0.4.14](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.4.13...arm_vgic-v0.4.14) - 2026-06-23

### Other

- updated the following local packages: ax-kspin

## [0.4.13](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.4.12...arm_vgic-v0.4.13) - 2026-06-22

### Other

- updated the following local packages: axvm-types, axdevice_base

## [0.4.12](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.4.11...arm_vgic-v0.4.12) - 2026-06-09

### Other

- Refactor Axvisor to unify ArceOS API and improve modularity ([#1019](https://github.com/rcore-os/tgoskits/pull/1019))

## [0.4.11](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.4.10...arm_vgic-v0.4.11) - 2026-06-03

### Other

- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.4.10](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.4.9...arm_vgic-v0.4.10) - 2026-05-22

### Other

- updated the following local packages: ax-errno, axaddrspace, axdevice_base

## [0.4.9](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.4.8...arm_vgic-v0.4.9) - 2026-05-19

### Other

- updated the following local packages: ax-errno, axaddrspace, axdevice_base

## [0.4.8](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.4.7...arm_vgic-v0.4.8) - 2026-05-18

### Other

- updated the following local packages: axaddrspace, axdevice_base

## [0.4.7](https://github.com/rcore-os/tgoskits/compare/arm_vgic-v0.4.6...arm_vgic-v0.4.7) - 2026-05-15

### Other

- *(arm-vgic)* inherit workspace metadata
