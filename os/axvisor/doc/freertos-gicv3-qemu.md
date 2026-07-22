# FreeRTOS GICv3 QEMU Guest

This document describes how to reproduce the FreeRTOS Rhealstone benchmark guest on Axvisor AArch64 QEMU.

## Prerequisites

- tgoskits workspace: `/home/ajax/Desktop/Project/Kernel/tgoskits`
- FreeRTOS guest tree: `/home/ajax/Desktop/Project/Kernel/freertos-guest`
- FreeRTOS kernel tree: `/home/ajax/Desktop/Project/Kernel/FreeRTOS-Kernel`
- AArch64 cross toolchain providing `aarch64-linux-musl-gcc`, `aarch64-linux-musl-ld`, and `aarch64-linux-musl-objcopy`

The Axvisor VM config expects these guest files:

- `/home/ajax/Desktop/Project/Kernel/freertos-guest/freertos.gicv3.qemu.bin`
- `/home/ajax/Desktop/Project/Kernel/freertos-guest/freertos-gicv3.dtb`

## Build The Guest

Build the default GICv3 guest with physical timer PPI 30:

```bash
make clean
make MEM_BASE=0x3f000000
cp freertos.bin freertos.gicv3.qemu.bin
```

Run these commands from:

```bash
/home/ajax/Desktop/Project/Kernel/freertos-guest
```

The default build is equivalent to:

```bash
make MEM_BASE=0x3f000000 GIC_VERSION=3 TIMER=30
```

`TIMER=30` uses `CNTP_TVAL_EL0`/`CNTP_CTL_EL0` and PPI 30, which matches the current Axvisor vtimer emulation path.

To build the virtual timer variant for future testing, use:

```bash
make clean
make MEM_BASE=0x3f000000 TIMER=27
```

`TIMER=27` uses `CNTV_CVAL_EL0`/`CNTV_CTL_EL0` and PPI 27.

## Run Axvisor

Run from the tgoskits workspace root:

```bash
timeout 90s cargo xtask axvisor qemu \
  --arch aarch64 \
  --smp 4 \
  --vmconfigs os/axvisor/configs/vms/qemu/aarch64/freertos-gicv3-smp1.toml
```

The host QEMU config uses GICv3:

```toml
-machine virt,virtualization=on,gic-version=3
```

## Expected Output

The guest should print the Rhealstone benchmark banner and complete all four tests:

```text
[FreeRTOS] Rhealstone Benchmark Suite

===== Rhealstone Benchmark =====
[1/4] Task Switch...
[2/4] Preemption...
[3/4] IRQ Latency + Tick Jitter (5s)...
[4/4] Semaphore Shuffle...

===== Done =====
```

After `===== Done =====`, the guest issues a system shutdown call and Axvisor should stop VM 1 normally.

## Notes

- The guest is linked for `0x3f80_0000`, so the VM config loads and enters the kernel at `0x3f80_0000`.
- The guest RAM region starts at `0x3f00_0000`.
- The default timer path uses PPI 30 because the current Axvisor vtimer implementation provides a physical timer sysreg path.
- The benchmark still uses `CNTVCT_EL0` for timestamp reads so timing statistics stay on the virtual counter.
