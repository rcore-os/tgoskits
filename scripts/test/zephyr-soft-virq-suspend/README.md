# Zephyr suspend-idle software-vIRQ consumer (E1 scenario)

## Purpose

This workload keeps every vCPU parked on the hypervisor's host-side wait
queue using PSCI `CPU_SUSPEND(standby)`, which makes the vIRQ notify/wake
path load-bearing. vCPU0 idles forever; vCPU1 runs a consumer thread pinned
to CPU1 and counts software IRQ 48. A single host injector targets vCPU1.
The scenario measures whether an interrupt targeted at vCPU1 wakes the
unrelated idle vCPU0.

## Requirements

- A Zephyr workspace with `west` and the Zephyr SDK. The build uses the
  `qemu_cortex_a53/qemu_cortex_a53/smp` board variant and the
  `CONFIG_SMP` / `CONFIG_SCHED_CPU_MASK` settings in `prj.conf`.
- The AxVisor host build (see the repository's axvisor QEMU instructions).
- A QEMU virt rootfs image that contains the built `zephyr.bin` at the path
  referenced by `axvisor-qemu-aarch64-suspend-smp2.toml`.

## Build the guest

```bash
west build -p always \
  -b qemu_cortex_a53/qemu_cortex_a53/smp \
  scripts/test/zephyr-soft-virq-suspend \
  -d /tmp/zephyr-soft-virq-suspend-build
```

Copy the raw image into the rootfs image at
`/tmp/zephyr-soft-virq-suspend-build/zephyr/zephyr.bin` (for example with
`debugfs -w -R 'write <host path> <guest path>' <rootfs.img>`).

## Run

The host injector must be in E1 mode (single stream targeting vCPU1). Set
`E1_MODE = true` in `os/axvisor/src/realtime_probe.rs`, then:

```bash
FEATURES=openrace-realtime cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-aarch64.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-aarch64.toml \
  --vmconfigs scripts/test/zephyr-soft-virq-suspend/axvisor-qemu-aarch64-suspend-smp2.toml
```

## Success criteria

- The guest prints `SOFTWARE VIRQ COMPLETE streams=1 samples_each=300
  total=300`.
- The host injector reports `VIRQ_INJECT_COMPLETE ... errors=0`.
- The `E1_COUNTERS` line shows `vcpu0_wake=1` (the idle vCPU is not woken by
  vCPU1-targeted notifications) and `lr_skip=0` (no dropped edges).

## Log analysis

- `VIRQ_INJECT sequence=... requested_ns=...` are host-side request
  timestamps.
- `E1_COUNTERS` reports per-vCPU park/wake counts, per-vCPU notify wake
  counts, and GIC list-register skip count:
  - `vcpu0_wake`: times the idle vCPU returned from its host-side wait
  - `notify_woke0/1`: times a notify actually woke a parked vCPU
  - `lr_skip`: times an injection was deferred because the vector was
    already pending/active in a GIC list register

Guest ISR timestamps are emitted as `48,<sequence>,<timestamp_ns>` CSV lines
and can be matched against `requested_ns` with:

```bash
python3 scripts/test/virq_latency_stats.py --exact <log>
```
