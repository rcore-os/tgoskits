# AxVisor scheduling status for Task 1

AxVisor's default host scheduler is the cooperative FIFO scheduler
(`components/axsched/src/fifo.rs`). A guest vCPU remains on the physical CPU
until VM execution exits and the host regains control. Sharing one physical CPU
between unrelated vCPU tasks therefore does not provide a bounded dispatch
latency.

The selected, validated Task 1 configuration uses guest-vCPU partitioning instead of
depending on host time-slice preemption:

```sh
LIBCLANG_PATH=/usr/lib/llvm-14/lib \
  cargo +nightly-2026-07-15 xtask axvisor qemu --arch aarch64 \
  --config docs/realtime/axvisor-qemu-aarch64-partition.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-aarch64.toml
```

That profile builds for four host CPUs without `sched-rr`. Its Linux guest has
two vCPUs assigned to host CPU IDs 2 and 3 and declares those masks dedicated.
The current partition boundary prevents other registered guest vCPU tasks from
using those CPUs. It does not yet isolate the CPUs from every host kernel task
or interrupt.

The standalone benchmark topology above is different from the mixed IVC
topology. In the IVC run, Zephyr vCPU0 is dedicated to pCPU0, Linux vCPU0/1 are
dedicated to pCPU1/2, and pCPU3 is excluded from guest masks for intended
housekeeping. That does not prove that every AxVisor task or physical interrupt
is pinned to pCPU3.

## Final feature-off/feature-on measurements

The final QEMU TCG campaign compared the same implementation and two-vCPU
Linux workload with the partition policy disabled (`shared`) and enabled
(`partitioned`). It is not a historical unmodified-source before/after
comparison. Each normal case collected 10,000 samples per metric at 1 ms under
idle and verified CPU1 stress; a partitioned 10 ms soak measured three
100-second windows.

Under stress, partitioning reduced scheduler-dispatch p99 from 148,240 to
137,584 ns (7.19%) and maximum from 541,376 to 372,256 ns (31.24%). Periodic
jitter p99 improved from 245,120 to 237,264 ns, but jitter maximum worsened
from 694,096 to 944,512 ns, and the timer-IRQ proxy p99/maximum also worsened.
Idle showed the same mixed pattern: dispatch tails improved, while several
maxima did not. The soak's largest observed periodic jitter was 6,690,800 ns.

These observations support deterministic placement and selected dispatch-tail
improvement only. Summaries, provenance, compressed raw logs, CPU load, exact
intervals, and limitations are retained in
[`competition/results/axvisor-rt-reference`](../../competition/results/axvisor-rt-reference/).

## Round-robin status: experimental under passthrough

ArceOS provides `ax-sched::RRScheduler`, selected through the
`ax-std/sched-rr` feature in
`axvisor-qemu-aarch64-preempt-rr.toml`. A build and boot log such as:

```text
cargo build ... --features ...,ax-std/sched-rr,...
[ ax_task::api:150 ] use Round-robin scheduler.
[ axvm::vm:658 ] Booting VM[1]
```

only proves that the RR scheduler was selected and that the guest booted. It
does **not** prove that `MAX_TIME_SLICE` bounds guest execution. With passthrough
interrupt handling, a host scheduler tick is useful only if it forces a VM
exit, reaches the host timer/scheduler path, and causes another runnable task to
be dispatched. That complete path has not been demonstrated.

Consequently, RR remains experimental for passthrough guests and is not part of
the selected partition profile. It must not be described as effective guest
preemption until a test demonstrates all of the following:

1. A guest vCPU executes a non-yielding busy loop.
2. A periodic host timer causes bounded VM exits while that loop runs.
3. The RR tick expires the vCPU task's slice.
4. A second runnable host or vCPU task runs before the busy-loop guest exits
   voluntarily.
5. Trace timestamps or counters bound the exit-to-dispatch interval without
   relying on console output in the measured path.

The tracked benchmark harness in `scripts/benchmark/axvisor-rt` collects
guest-visible periodic jitter, scheduler-dispatch latency, and a virtual-timer
IRQ-response proxy. These measurements can compare configurations, but they do
not replace the forced-VM-exit proof above. QEMU TCG results are relative
engineering signals, not hardware real-time guarantees.

## Multi-VM boot is separate

Changing FIFO to RR does not establish that simultaneous multi-VM startup is
correct. VM creation, device initialization, and parallel vCPU bring-up have
their own ordering and resource constraints and require separate validation.
