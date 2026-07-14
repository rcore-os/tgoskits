# MAINTAINERS

This file is the local source of truth for PR reviewer routing.

Field meanings:

- `M:` maintainer or primary owner
- `R:` reviewer login to request on GitHub
- `F:` path or glob hint
- `K:` keyword or direction hint

## Architecture, Platform, And Drivers

M: @ZR233
R: @ZR233
F: os/arceos/
F: os/axvisor/
F: virtualization/axvm/
F: drivers/
F: components/axcpu/
K: `arceos`, `axvisor`, VM, guest boot, driver, VirtIO, PCI, MMIO, DMA, IRQ, USB, camera, robot, platform, boot, SMP, trap/context, architecture, aarch64, loongarch

## CI, Tests, Rootfs, And Repository Workflow

M: @ZCShou
R: @ZCShou
F: .github/
F: test-suit/
F: apps/
F: scripts/
F: docs/
K: CI, tests, `test-suit`, QEMU runner, rootfs, distro, `axbuild`, repo maintenance, docs, release, workflow

## x86 Virtualization, Filesystem, Guest Communication, And Scheduling

M: @Josen-B
R: @Josen-B
F: virtualization/x86_vcpu/
F: virtualization/x86_vlapic/
F: components/axfs-ng-vfs/
F: components/rsext4/
F: components/axsched/
F: os/arceos/modules/axfs-ng/
K: `x86_vcpu`, x86_64 virtualization, VMX, SVM, VMCS, VMCB, Linux/UEFI guest boot, PIT handling, IVC/HVC, guest communication, FreeRTOS/Zephyr guest, host-fs, `axfs-ng-vfs`, `rsext4`, ext4, `axsched`, `BaseScheduler`, FIFO/RR/CFS, `sched-rr`, `sched-cfs`

## SD/MMC, Syscall, And RISC-V

M: @YanLien
R: @YanLien
F: drivers/blk/sdmmc-protocol/
F: drivers/blk/sdhci-host/
F: drivers/blk/dwmmc-host/
F: drivers/blk/cv181x-sdhci/
F: drivers/blk/starfive-jh7110-dwmmc/
F: drivers/ax-driver/src/block/
F: components/sdio-host/
F: components/sdio-host2/
F: components/sdhci-cv1800/
F: os/arceos/api/arceos_posix_api/
F: os/arceos/ulib/axlibc/
F: virtualization/riscv_vcpu/
F: virtualization/riscv_vplic/
F: virtualization/riscv-h/
F: test-suit/**/qemu-riscv64.toml
F: test-suit/**/*riscv*
K: SD/MMC, SDHCI, DWMMC, `sdmmc`, `k230-sdhci`, `rockchip-sdhci`, `starfive-jh7110-dwmmc`, `simple-sdmmc`, `mmcblk`, `vmmc-supply`, `vqmmc-supply`, syscall, `sys_*`, `ax_posix_api`, `axlibc`, riscv64, `riscv64gc-unknown-none-elf`, `qemu-riscv64`, `riscv_vcpu`, `riscv_vplic`, SBI/OpenSBI, guest timer, runtime IPI

## Memory Management, Address Spaces, And Page Tables

M: @bullhh
R: @bullhh
F: memory/
F: os/arceos/modules/axmm/
F: os/arceos/modules/axalloc/
F: virtualization/axaddrspace/
F: virtualization/axvm/src/layout.rs
F: virtualization/x86_vcpu/src/ept.rs
K: memory management, address space, page table, paging, `ax-mm`, `axaddrspace`, `page-table-generic`, `ax-page-table-multiarch`, `ax-page-table-entry`, `ax-memory-set`, `ax-memory-addr`, `axalloc`, `AddrSpace`, `KERNEL_ASPACE`, `PageTable`, `PageTableCursor`, `FrameAllocator`, `PagingHandlerImpl`, `MappingFlags`, `MemRegionFlags`, `Backend::Allocation`, `mmap`, `munmap`, `mprotect`, `brk`, user memory, EPT/NPT, Stage-2, nested page table, `NestedPagingConfig`, GPA/GVA
