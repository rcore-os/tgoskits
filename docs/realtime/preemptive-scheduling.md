# Preemptive (round-robin) scheduling for AxVisor — Task 1 (RT direction)

AxVisor's default host scheduler is the **cooperative, non-preemptive FIFO**
(`components/axsched/src/fifo.rs`: `task_tick` returns `false`). A busy vCPU
task never yields, which is the opposite of what a real-time hypervisor wants.

ArceOS already ships preemptive schedulers (`ax-sched::RRScheduler` with a time
slice, and `CFScheduler`), selected by the `sched-rr` / `sched-cfs` cargo
features (which pull in `preempt` → `irq`). They are not enabled in the stock
QEMU aarch64 AxVisor build.

## Enabling preemptive RR

Build with the `ax-std/sched-rr` feature (see
`axvisor-qemu-aarch64-preempt-rr.toml`):

```
cargo xtask axvisor qemu --arch aarch64 \
  --config docs/realtime/axvisor-qemu-aarch64-preempt-rr.toml \
  --vmconfigs <vm>.toml
```

## Verified

The build resolves the round-robin scheduler and runs:

```
cargo build ... --features ...,ax-std/sched-rr,...
[ ax_task::api:150 ]   use Round-robin scheduler.
[ axvm::vm:658 ] Booting VM[1]
... guest boots to userspace, eth0 up, application server running ...
```

So AxVisor now runs the guest under a **preemptive, time-sliced (MAX_TIME_SLICE
= 5) round-robin scheduler** instead of cooperative FIFO — the scheduling/
preemption change Task 1 calls for. This is the software foundation for RT
behavior; the quantitative before/after latency/jitter comparison (cyclictest
periodic jitter, scheduling-latency, IRQ-response, max-latency, long-run
stability under stress-ng/hackbench/fio, and an RTOS baseline) must be measured
on real hardware, since QEMU/TCG is not cycle-accurate and yields no meaningful
RT numbers. Baselines collected so far (QEMU, relative only) are in
`M1-zephyr-baseline.md`.

## Note on multi-VM boot

Switching FIFO → RR does **not** fix the unreliable simultaneous 2-VM boot seen
in the inter-guest work (`../ivc/M5-network-design.md`): the two guest vCPUs are
pinned to separate pCPUs, so the contention is in AxVisor's parallel VM/vCPU
bring-up path, not intra-pCPU time-slicing. That remains a separate item.
