# Axvisor realtime CPU partitioning

## Problem and goal

On a four-core target, Axvisor must run StarryOS vCPU tasks on three physical
CPUs while one physical CPU runs host realtime tasks. Realtime task creators
must not know the selected CPU or construct a CPU mask. A build with no
realtime workload must retain all CPUs for ordinary work.

Success means that the build selects zero or one realtime CPU, ordinary and
vCPU tasks cannot execute there, realtime tasks are pinned there before their
first enqueue, and invalid topology fails during scheduler initialization.

## Configuration and invariants

`realtime_cpu_id` is declared in the Axvisor build TOML. `-1` (the default)
means disabled; a nonnegative integer selects that logical CPU. Axbuild passes
the value into the `ax-task` compile-time configuration. Other negative values
and IDs outside the build-time CPU capacity are build errors. At boot, the
selected ID must be online and must not be the primary CPU.

The generated runtime representation is `Option<usize>`; the `-1` sentinel is
confined to the build boundary. The ordinary CPU mask is the online CPU set
minus the realtime CPU. VM configuration must reject a physical CPU set that
intersects the realtime CPU rather than silently remapping it.

## Scheduling and API

All CPUs initialize `axtask`, preserving the existing SMP, interrupt and IPI
startup contracts. With `sched-rt-fifo`, positive priorities are strict FIFO
realtime priorities. Priority zero is ordinary work and rotates on timer ticks.

`spawn_realtime(entry, name, stack_size, priority)` obtains the configured CPU
internally, installs its one-bit affinity and positive priority before the task
is registered or enqueued, and returns `RealtimeDisabled` or `InvalidPriority`
for caller-correctable errors. It deliberately has no CPU-mask parameter.

## Isolation boundary

The first stage provides scheduler/placement isolation, not full temporal or
memory isolation. Device interrupts, global locks, shared cache/memory buses,
firmware interrupts and console output can still add jitter. IRQ routing and
device ownership must be audited per platform before claiming hard realtime.

## Alternatives

An independent executor on the realtime CPU offers a smaller runtime surface,
but conflicts with realtime work that depends on `axtask` services. Dynamic CPU
borrowing improves utilization but weakens the static isolation contract. This
design therefore keeps a restricted `axtask` run queue and static ownership.

## Validation

Unit tests cover configuration parsing and RT-FIFO ordering/preemption. The
checked-in QEMU smoke test uses four CPUs, reserves CPU 3, and runs the host
realtime workload without a guest. It reports warmup and measured sample counts,
maximum and percentile latency, and missed deadlines. Physical-board validation
remains required for hard-realtime claims because QEMU cannot model interrupt
and memory-system interference.

The QEMU AMP manifest is under
`test-suit/axvisor/normal/qemu-amp/host-rt`. Run it with:

```sh
cargo xtask axvisor test qemu \
  --arch aarch64 -g normal -c qemu-amp/host-rt
```

The combined AArch64 case builds StarryOS, boots it with one vCPU on CPU 0,
and keeps CPU 3 for the host realtime task:

```sh
cargo xtask starry build \
  --config test-suit/axvisor/guest-build/starry-aarch64-amp.toml --smp 1
cargo xtask axvisor test qemu \
  --arch aarch64 -g normal -c qemu-amp/starry-host-amp
```

Its single success expression requires both the Starry shell marker and the
host latency result. The guest uses partial-passthrough GICD/GICR, retains the
firmware `/chosen` and `/aliases` console contract, and places runtime RAM in a
reserved identity-mapped region so a physically passed-through NVMe device can
DMA safely. The finite benchmark task remains resident after reporting because
the dedicated realtime CPU is outside the ordinary scheduler domain.

The host-only case deliberately has no external guest-image dependency.
FreeRTOS comparison remains separate follow-up validation work.

## RK3588 / OrangePi 5 Plus

The board build uses the same compile-time partition as QEMU and reserves
logical CPU 3 on the eight-core RK3588. It reuses the maintained OrangePi
Starry VM configuration and enables the Rockchip SDHCI/MMC drivers:

```sh
cargo xtask axvisor build \
  --arch aarch64 \
  --config test-suit/axvisor/normal/board-orangepi-5-plus/starry-host-amp/build-aarch64-unknown-none-softfloat.toml
cargo xtask axvisor test board \
  --board orangepi-5-plus-starry-host-amp
```

The board case is a deployment/build recipe; it must be run with an
OrangePi-5-Plus board lease and the board's Linux rootfs/guest assets prepared
according to the existing board guide. QEMU results must not be presented as
RK3588 timing evidence. Board acceptance must additionally collect the
`AMP_RT_RESULT` line under idle, guest CPU, storage and network pressure.

## Rollback

Setting `realtime_cpu_id = -1` (or omitting it) disables realtime task creation
and restores the full ordinary CPU mask. Disabling `sched-rt-fifo` restores the
prior global scheduler selection.
