# Zephyr suspend-idle software-vIRQ consumer (E1 scenario)

## Purpose

This workload keeps every vCPU parked on the hypervisor's host-side wait
queue using PSCI `CPU_SUSPEND(standby)`, which makes the vIRQ notify/wake
path load-bearing. vCPU0 idles forever; vCPU1 runs a consumer thread pinned
to CPU1 and counts software IRQ 48. A single host injector targets vCPU1.
The scenario measures whether an interrupt targeted at vCPU1 wakes the
unrelated idle vCPU0.

## Requirements

- Python 3 with `pip`, plus `west`:

  ```bash
  # If pip is missing (e.g. a minimal container image):
  python3 -m ensurepip --upgrade || sudo apt-get install -y python3-pip
  python3 -m pip install --user west
  export PATH="$HOME/.local/bin:$PATH"
  ```

- A Zephyr workspace pinned to revision
  `aa37fa1ebc925c1f58c7d345c724433c89368ed2`:

  ```bash
  west init -m https://github.com/zephyrproject-rtos/zephyr.git /tmp/zephyrproject
  cd /tmp/zephyrproject
  git -C /tmp/zephyrproject/zephyr checkout aa37fa1ebc925c1f58c7d345c724433c89368ed2
  west update
  export ZEPHYR_BASE=/tmp/zephyrproject/zephyr
  ```

- The Zephyr SDK, installed and verified:

  ```bash
  west sdk install -d /tmp/zephyr-sdk   # downloads the default SDK version
  export ZEPHYR_SDK_INSTALL_DIR=/tmp/zephyr-sdk
  test -f "$ZEPHYR_SDK_INSTALL_DIR/sdk_version" && echo "SDK ready"
  ```

  The build uses the `qemu_cortex_a53/qemu_cortex_a53/smp` board variant and
  the `CONFIG_SMP` / `CONFIG_SCHED_CPU_MASK` settings in `prj.conf`.
- The AxVisor host build (see the repository's axvisor QEMU instructions).
- A QEMU virt rootfs image (for example one produced by the repository's
  axbuild flow) that can boot aarch64 Linux.

## Build the guest

```bash
ZEPHYR_BASE=/tmp/zephyrproject/zephyr \
ZEPHYR_SDK_INSTALL_DIR=/tmp/zephyr-sdk \
west build -p always \
  -b qemu_cortex_a53/qemu_cortex_a53/smp \
  scripts/test/zephyr-soft-virq-suspend \
  -d /tmp/zephyr-soft-virq-suspend-build
```

Copy the raw image into the rootfs image and verify it is readable by the
guest at the path referenced by the committed VM configuration:

```bash
ROOTFS=</path/to/rootfs.img>
debugfs -w -R \
  'write /tmp/zephyr-soft-virq-suspend-build/zephyr/zephyr.bin /tmp/zephyr-soft-virq-suspend-build/zephyr/zephyr.bin' \
  "$ROOTFS"
debugfs -R 'stat /tmp/zephyr-soft-virq-suspend-build/zephyr/zephyr.bin' "$ROOTFS"
```

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

Readiness check on the captured serial log:

```bash
rg 'SOFTWARE VIRQ COMPLETE streams=1 samples_each=300 total=300' <log>
rg 'VIRQ_INJECT_COMPLETE .*errors=0' <log>
rg 'E1_COUNTERS .*vcpu0_wake=1.*lr_skip=0' <log>
```

## Example run (current head)

Captured with this commit on a QEMU virt AArch64 host:

```text
consumer pinned to cpu 1 rc=0
VIRQ_INJECT_COMPLETE vm=2 vcpu=1 vector=48 samples=300 errors=0
E1_COUNTERS vcpu0_park=2 vcpu0_wake=1 vcpu1_park=174 vcpu1_wake=174 notify_woke0=0 notify_woke1=61 lr_skip=0
SOFTWARE VIRQ COMPLETE streams=1 samples_each=300 total=300
```

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
