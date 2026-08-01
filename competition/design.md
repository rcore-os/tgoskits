# System and protocol design

## 1. Scope and assurance boundary

The system targets the three required functions:

1. deterministic guest-vCPU placement and a two-vCPU Linux guest;
2. bidirectional Linux-to-RTOS communication over an IP network; and
3. neural inference in Linux driving an observable RTOS control loop.

The selected, validated scheduling profile uses CPU partitioning with the existing FIFO
host scheduler. It does not claim that AxVisor currently preempts a
non-yielding passthrough guest on a bounded time slice. The repository's
round-robin profile remains experimental because a timer-driven VM-exit to
host-dispatch path has not been proven. See
[`docs/realtime/preemptive-scheduling.md`](../docs/realtime/preemptive-scheduling.md).

Similarly, the software partition excludes other *registered guest vCPU
tasks* from Linux's dedicated masks. It does not yet prove that all AxVisor
kernel tasks or physical interrupts are excluded from those CPUs. QEMU TCG
measurements are suitable for relative regression comparisons, not hardware
real-time guarantees.

## 2. Architecture

```text
                         QEMU virt, AArch64, GICv3, 4 pCPUs, 8 GiB
+----------------------------------------------------------------------------+
| AxVisor                                                                    |
|                                                                            |
|  CPU partition planner                  isolated software Ethernet switch   |
|  - validates all VMs before start       - fixed per-port MAC and segment    |
|  - reserves dedicated masks             - source anti-spoofing              |
|  - prunes shared masks                   - exact known-unicast forwarding    |
|  - fails closed on conflicts             - bounded same-segment multicast    |
|                                                                            |
|  +-----------------------------+        +-------------------------------+   |
|  | Linux guest, VM 1           |        | Zephyr guest, VM 2            |   |
|  | vCPU0 -> pCPU1 dedicated    |        | vCPU0 -> pCPU0 dedicated      |   |
|  | vCPU1 -> pCPU2 dedicated    |        | 128 MiB allocated mapping     |   |
|  | 256 MiB identity mapping    |        | virtio-mmio net, IRQ 64       |   |
|  | virtio-mmio net, IRQ 56     |        | 10.0.0.2/24 UDP :5500         |   |
|  | 10.0.0.1/24                |        | protocol + actuator + plant   |   |
|  | neural/manual controller    |        +-------------------------------+   |
|  +-----------------------------+                    ^                       |
|             | CONTROL                               | STATUS, ACK, ERROR     |
|             +---------- UDP/IPv4, segment 1 --------+                       |
|                                                                            |
|  pCPU3 is intentionally left out of guest affinity masks for housekeeping. |
+----------------------------------------------------------------------------+
```

The outer QEMU machine exposes the Linux root image as a virtio block device
for AxVisor's filesystem support. QEMU is configured with `-net none`; all
guest Ethernet traffic is delivered by AxVisor's emulated virtio-net devices
and internal switch.

## 3. Guest and platform configuration

The full profile is composed from:

- [`axvisor-aarch64.toml`](ivc/config/axvisor-aarch64.toml): AArch64 target,
  four CPUs, the current 100 Hz `ax-runtime` default tick, filesystem and
  virtio-block support;
- [`qemu-aarch64.toml`](ivc/config/qemu-aarch64.toml): Cortex-A72, QEMU `virt`,
  GICv3, multi-threaded TCG, four CPUs, 8 GiB, no host NIC;
- [`linux-smp2.toml`](ivc/config/linux-smp2.toml): neural Linux controller VM;
- [`linux-smp2-manual.toml`](ivc/config/linux-smp2-manual.toml): otherwise
  identical 500-permille manual baseline VM;
- [`linux-smp2-ack-loss.toml`](ivc/config/linux-smp2-ack-loss.toml): finite
  100-command neural fault campaign;
- [`zephyr-smp1.toml`](ivc/config/zephyr-smp1.toml): normal Zephyr endpoint VM;
  and
- [`zephyr-smp1-ack-loss.toml`](ivc/config/zephyr-smp1-ack-loss.toml) plus
  [`ack-loss.conf`](ivc/zephyr/ack-loss.conf): otherwise identical Zephyr image
  that suppresses selected first ACKs.

| Resource | Linux VM 1 | Zephyr VM 2 |
| --- | --- | --- |
| vCPUs | 2 | 1 |
| Requested pCPU mask | vCPU0 `0x2` (pCPU1), vCPU1 `0x4` (pCPU2) | vCPU0 `0x1` (pCPU0) |
| Partition policy | dedicated | dedicated |
| Guest memory | `0x80000000..0x8fffffff`, 256 MiB, identity map | `0x40000000..0x47ffffff`, 128 MiB, allocated map |
| Entry/load address | `0x80200000` | `0x40000000` |
| DTB address | `0x80000000` | `0x47e00000` |
| Image | `/guest/linux/linux-qemu` in AxVisor filesystem | `ivc/zephyr/build/zephyr/zephyr.bin` |
| virtio-net MMIO | `0x0a001000`, 4 KiB | `0x0a002000`, 4 KiB |
| Guest interrupt | architectural INTID 56 | architectural INTID 64 (DTS `GIC_SPI 32`) |
| MAC | `52:54:00:00:00:01` | `52:54:00:00:00:02` |
| IPv4 | `10.0.0.1/24` | `10.0.0.2/24` |
| UDP | ephemeral controller port -> `10.0.0.2:5500` | listen on `10.0.0.2:5500` |
| Switch segment | 1 | 1 |

Linux boots read-only from `/dev/vda` with `/ivc-init.sh`, configures `eth0`,
adds only the connected `10.0.0.0/24` route, and starts the controller. The
default run is neural mode, 1,800 samples, and a nominal 100 ms period.

The exact normal Linux kernel command line is:

```text
console=ttyAMA0,115200 earlycon=pl011,0x09000000 root=/dev/vda ro init=/ivc-init.sh loglevel=7 ivc.mode=neural ivc.count=1800 ivc.period_ms=100
```

Manual changes only `ivc.mode=manual`; ACK-loss uses neural mode with
`ivc.count=100`. The outer QEMU virtio block image is mounted by AxVisor's
filesystem layer so the Linux kernel can be loaded from
`/guest/linux/linux-qemu`; Linux receives its own identity-mapped 256 MiB
region and an emulated virtio-net MMIO device. Zephyr is loaded directly from
its raw binary into a separate allocated 128 MiB region. Both VM descriptions
use `passthrough_devices = [["/"]]` to supply a guest device tree rooted at
`/`, while their explicit emulated MMIO devices and INTIDs remain distinct.
The passthrough interrupt mode routes Linux INTID 56 and Zephyr INTID 64
through the AArch64 physical-SPI ownership gate described below.

Zephyr targets upstream `qemu_cortex_a53` v4.3.0. Its device-tree overlay
enables virtio-mmio slot 16 and fixes the link address. Startup rejects a
runtime MAC mismatch before binding the UDP socket.

The virtio-net backend uses Linux-compatible feature negotiation: the 10-byte
header is limited to a legacy driver that accepts neither
`VIRTIO_NET_F_MRG_RXBUF` nor `VIRTIO_F_VERSION_1`; accepting either feature
selects the modern 12-byte layout. Upstream Zephyr v4.3.0 accepts VERSION_1 and
uses 12 bytes without accepting MRG_RXBUF, so its VM also opts into the
documented `cfg_list[2] = 1` fixed-header mode to pin that integration contract.
The Linux VM remains on the negotiated path.

### CPU-partition invariants

The planner consumes all VM placements before any vCPU task affinity is used:

- a dedicated vCPU mask must be present, nonzero, and contained in the online
  pCPU mask;
- dedicated VM masks must not overlap;
- every shared vCPU mask is pruned against the union of dedicated masks;
- an empty effective shared mask is an error, not a scheduler-default fallback;
- VM registration order does not change the result; and
- the planner uses maximum matching to choose unique initial pCPUs instead of
  making a greedy order-dependent choice;
- the guest FDT exposes exactly the enabled vCPU set and the selected initial
  placement;
- vCPU tasks are prepared before any are activated, then activation rechecks
  the current online CPU mask and effective affinity;
- a failed activation rolls back all tasks prepared for that VM; and
- the validated registry is frozen when runtime task lookup begins.

These rules prevent a later-registered VM from silently restoring access to a
dedicated CPU, and prevent an online-CPU change between planning and activation
from silently weakening the placement contract.

### AArch64 passthrough SPI ownership

The emulated virtio-net devices signal physical GICv3 SPIs because these guests
use passthrough interrupt mode. That path has an explicit host/guest ownership
contract:

- the EL2 host initially configures its CPU interface for split
  EOI/deactivation, while the passthrough guests complete interrupts with
  `EOIR` alone; before the first passthrough guest entry on a pinned CPU, AxVM
  changes that interface to combined-EOI mode so the guest cannot leave the
  first SPI permanently active;
- `ArmVcpu::run` saves the caller's complete DAIF state, masks host IRQs, invokes
  the before-entry and after-exit hooks while IRQs remain masked, and restores
  the exact saved DAIF state instead of unconditionally enabling interrupts;
- every vCPU owns a preallocated SPI gate with `Host` and `Guest` phases. The
  lock order is the global GIC lock followed by the per-vCPU gate;
- while the host owns the interface, a device signal is queued and the target
  vCPU task is notified. Guest entry validates the whole batch, routes only
  inactive SPIs, enables them, and makes them pending after all fallible route
  work succeeds;
- while the guest owns the interface, a signal uses the already armed route and
  pends the physical SPI without sending a host IPI. On VM exit, pending state
  is reclaimed before ownership returns to the host; an active route is
  preserved across re-entry; and
- an emulated passthrough SPI requires a vCPU with one enabled, pinned physical
  CPU. The current check fails the first device interrupt rather than VM
  registration, so a future boot-time validation would improve diagnostic
  timing without changing the safety boundary.

This gate prevents the host from acknowledging a guest-targeted SPI in the
entry/exit race window. It does not prove bounded physical interrupt latency,
exclude unrelated host interrupts from dedicated CPUs, or make a migratable
passthrough CPU interface safe.

### Real-time validation result

The standalone Task 1 harness uses the same two-vCPU Linux image in two policy
profiles on one four-pCPU Cortex-A72 QEMU TCG machine. `shared` lets both vCPUs
use pCPUs 0-3 (initial placement 0/1); `partitioned` gives vCPU0 only pCPU2 and
vCPU1 only pCPU3. This is a same-source feature-off/feature-on comparison, not
a historical unmodified-`dev` binary baseline.

Every normal row below has 10,000 post-warm-up samples per metric at 1 ms. The
soak has 10,000 samples at 10 ms per metric, giving three 100-second measured
windows. Values are p99/maximum nanoseconds; load is guest CPU0/CPU1 busy time
from paired `/proc/stat` records.

| Profile/workload | Guest load | Jitter | Dispatch | Timer-IRQ proxy |
| --- | ---: | ---: | ---: | ---: |
| shared idle | 35.563% / 0.142% | 236,864 / 1,183,648 | 164,704 / 398,832 | 229,424 / 433,488 |
| shared stress | 2.124% / 100.000% | 245,120 / 694,096 | 148,240 / 541,376 | 226,608 / 438,800 |
| partitioned idle | 36.301% / 0.176% | 231,328 / 1,222,368 | 154,080 / 372,928 | 225,056 / 1,298,736 |
| partitioned stress | 1.995% / 100.000% | 237,264 / 944,512 | 137,584 / 372,256 | 240,832 / 454,880 |
| partitioned stress soak | 1.667% / 100.000% | 333,072 / 6,690,800 | 145,440 / 275,760 | 280,720 / 6,388,560 |

| Capture | UTC interval | Uncompressed raw-log SHA-256 |
| --- | --- | --- |
| shared idle | `2026-07-30T23:34:37Z` - `23:41:54Z` | `638c72d723ead40f7f4ca2ae5fb7362219c95e8bd9b482588035848f155003fd` |
| shared stress | `2026-07-30T23:51:12Z` - `2026-07-31T00:01:08Z` | `5179ad02eba344606dff53853c312b295b89b7ae89135697fa68c194655590cc` |
| partitioned idle | `2026-07-31T00:02:08Z` - `00:09:31Z` | `0010d39af45494b01e359d9ddb9b85553591431ae5592bc8d608d169e5434d37` |
| partitioned stress | `2026-07-31T00:10:20Z` - `00:20:18Z` | `9361542d542a141462c1504d12cc450438e2f05fce0c5bd044731de7aff4d76c` |
| partitioned stress soak | `2026-07-31T00:21:24Z` - `00:34:24Z` | `729a04ad0572a14c0c268910dc73e739a40709f4f508835827e0b4f3767883c2` |

In the paired stress runs, partitioning improved dispatch p99/maximum by
7.19%/31.24% and jitter p99 by 3.20%, but jitter maximum and both timer-IRQ
proxy tails worsened. Idle dispatch p99/maximum improved by 6.45%/6.49%, while
the idle timer-IRQ maximum worsened sharply. The structural isolation and
selected dispatch-tail improvements are supported; universal latency
improvement is not. The retained evidence is under
[`results/axvisor-rt-reference`](results/axvisor-rt-reference/).

## 4. Network isolation and access control

The emulated device configuration values are
`[MAC suffix, segment ID, optional header compatibility]`. Thus `[1, 1]` and
`[2, 1, 1]` produce the two MAC addresses above and place both ports in segment
1; the Zephyr-only final value selects the compatibility mode described above.

Each forwarding pass builds a bounded topology snapshot (at most 256 ports)
and applies these rules:

- port IDs are unique and per-segment MAC addresses are unique;
- configured port MACs must be nonzero unicast addresses;
- the source MAC must equal the ingress port's configured MAC;
- known unicast reaches exactly one port in the ingress segment;
- unknown and reflected unicast is dropped rather than flooded;
- broadcast and multicast reach only other ports in the ingress segment; and
- topology/allocation/destination-buffer failures are contained and counted.

The switch exposes counters for transmitted frames/bytes, unicast and
multicast decisions, forwarding attempts/copies, each policy drop reason,
topology failures, unavailable receive buffers, and delivery errors. These are
observability counters only; relaxed atomics do not publish switch state.

No bridge, NAT, or firewall rule is needed because this profile has no
host-facing link. The segment and anti-spoof policy are the access-control
boundary. Adding an external NIC later requires a separate explicit route,
firewall policy, and threat review.

vsock is not used. Shared memory, hypercalls, and bare MMIO are not application
data channels.

## 5. IVC/1 application protocol

One IVC frame occupies one UDP datagram. Multi-byte integers are little-endian.
The maximum application payload is 1,200 bytes, keeping the 1,232-byte maximum
frame below a conventional Ethernet MTU.

### Fixed 32-byte header

| Offset | Size | Field | Rule |
| --- | ---: | --- | --- |
| 0 | 4 | magic | ASCII `IVC1` |
| 4 | 1 | version | `1` |
| 5 | 1 | message type | `1` CONTROL, `2` STATUS, `3` ERROR, `4` ACK, `5` TELEMETRY |
| 6 | 2 | flags | bit 0 ACK-required, bit 1 retransmission; other bits rejected |
| 8 | 4 | session ID | zero is reserved |
| 12 | 4 | sequence | zero is reserved |
| 16 | 8 | sender timestamp | monotonic microseconds in the sender's clock domain |
| 24 | 2 | payload length | exact datagram length must be `32 + length` |
| 26 | 2 | error code | zero except ERROR frames; ERROR must be nonzero |
| 28 | 4 | checksum | CRC-32/IEEE of header and payload with these four bytes zeroed |

Both implementations run the same checked-in golden frame and CONTROL payload
vector. This catches byte-order, layout, and checksum drift before Zephyr
opens the socket.

### Payloads

CONTROL is 12 bytes:

| Offset | Size | Field |
| --- | ---: | --- |
| 0 | 1 | operation: set actuator, enter safe state, or heartbeat |
| 1 | 1 | mode: safe, manual fixed, or neural |
| 2 | 2 | actuator command, `0..=1000` permille |
| 4 | 4 | signed setpoint in milli-degrees Celsius, `-40000..=150000` |
| 8 | 4 | monotonically increasing sample ID |

STATUS is 20 bytes: state and active mode (one byte each), actuator permille
(two bytes), measured temperature, setpoint, and applied sequence (four bytes
each), fault code (two bytes), then two reserved zero bytes. States are ready,
applied, safe fallback, and fault.

ACK is 12 bytes: acknowledged sequence, next expected sequence, and the low 32
bits of the receive-window mask. ERROR is 8 bytes: offending message type,
three zero reserved bytes, and offending sequence. TELEMETRY is reserved by
the type registry but is not part of the current control loop.

Error codes distinguish malformed frame, unsupported version, checksum
mismatch, sequence outside the window, invalid or stale control, actuator
range, controller timeout, and internal failure.

## 6. UDP reliability and safety

The Linux controller uses one in-flight command at a time. Its current runtime
configuration waits 100 ms for a response and allows up to 20 retransmissions
after the original send. A command completes only after both matching STATUS
and ACK arrive. Session ID and sequence must match; unexpected responses are
counted as protocol errors.

The receiver keeps a fixed 64-sequence window per session:

- a new in-order command is applied exactly once;
- a duplicate returns the current STATUS and ACK without applying again;
- an out-of-order packet is recorded, but the current endpoint rejects it with
  `SequenceOutsideWindow` rather than applying out of order;
- a sequence beyond the bounded window is rejected; and
- only sequence 1 may begin a different nonzero session;
- replacing the current session retires its ID in an eight-entry bounded ring;
  delayed datagrams from a retained session are rejected instead of replacing
  a restarted controller; and
- a fresh controller session begins at sequence 1 and resets the active
  receive-window state.

The deterministic fault image suppresses only the first ACK for every fifth
fresh sequence. It still returns STATUS, so the Linux timeout retransmits the
same sequence. Zephyr recognizes that datagram as a duplicate and returns
current STATUS plus ACK without applying the actuator or advancing the plant a
second time. Normal images compile with both fault settings equal to zero.

Wire decode rejects truncated, oversized, length-mismatched, unknown-version,
unknown-flag, invalid-error, and bad-checksum datagrams before payload use.
Payload decode then enforces exact sizes, ranges, reserved zeros, and compatible
state/error combinations.

The endpoint enters safe mode with actuator 0 after more than 500,000 us
without a valid command. The reusable endpoint API rejects command ages above
250,000 us, but Linux and Zephyr monotonic clocks have unrelated epochs. The
Zephyr integration therefore supplies its local receive time as both age
endpoints and relies on session/sequence ordering for stale network datagrams;
it uses the same local clock for the silence timer. The sender timestamp remains
useful for sender-side round-trip measurement, but it must not be subtracted
directly from the Zephyr clock.

## 7. Neural control loop

`thermal-4x6x1-v1` is a dependency-free dense neural controller with four
normalized inputs, six ReLU hidden units, and one clamped output. Inputs are:

1. setpoint error;
2. setpoint relative to the 20 C ambient point;
3. measured temperature rate; and
4. previous actuator value.

Weights and biases are checked into
[`neural.rs`](../tools/ivcproto/src/neural.rs), so inference is deterministic
and requires no runtime model download or random number generator. The output
is converted to `0..=1000` actuator permille and encoded as a CONTROL payload.

The weights are hand-parameterized for this deterministic thermal plant; there
is no external training dataset or download pipeline. The compact 4x6x1
dense/ReLU form was selected to make inference observable, dependency-free,
`no_std`-compatible, and reproducible in a static Linux guest. The retained
controller binary hash binds the compiled weights. This is a neural inference
demonstration, not a claim of data-trained generalization.

The RTOS endpoint applies the command, advances a deterministic thermal plant,
and returns the resulting measured temperature and applied sequence in STATUS.
That status becomes the next Linux observation, forming the intended closed
loop:

```text
status/initial observation -> inference -> CONTROL/UDP -> RTOS actuator
        ^                                                   |
        +-------------------- STATUS/UDP <- plant step <-----+
```

The comparison baseline holds the actuator at 500 permille. The common
scenario uses 1,800 samples at 100 ms, setpoints 45/65/50 C for 60 seconds each,
and a `-0.35 C/s` disturbance during samples 850 through 949.

The controller timestamps every cycle on one Linux `Instant` clock before it
builds the observation or runs the selected policy, and stops after both the
matching STATUS and ACK are decoded. `full_loop_*` therefore includes policy
evaluation (including neural inference), payload/frame encoding, UDP/IP and
virtio transport, RTOS command application, the plant step, STATUS/ACK return,
and response decoding. `pre_send_*` isolates work through policy evaluation;
`transport_*` covers the remaining pre-send encoding plus request/response
path. This same-clock round trip avoids subtracting unrelated Linux and Zephyr
clock epochs. Reported resolution is one microsecond because nanosecond
`Instant` durations are truncated when serialized into the metric vectors.

## 8. Retained closed-loop and fault captures

The final neural and manual QEMU runs each completed 1,800/1,800 commands with
zero application errors, timeouts, retransmissions, RTOS duplicates, or RTOS
protocol errors:

| Policy | Source-log SHA-256 | Full-loop p50 / p95 / p99 / max | Throughput |
| --- | --- | ---: | ---: |
| Neural | `6c7f7e2e404a5c8ef8a9a3f632a24169b35d8be6a8c0ac496775bf9d32a07eb8` | 3,894 / 4,652 / 5,657 / 20,917 us | 9.962 msg/s |
| Manual fixed | `39ac8deaf5382490a007bfd47ec7384989c64c6092eed70ac8ff682c076d8a57` | 3,902 / 4,670 / 5,423 / 19,656 us | 9.963 msg/s |

The neural policy produced RMSE 5,932.491 mC and IAE 686,993.400 mC*s,
improving the manual values of 9,258.906 mC and 1,429,224.700 mC*s by 35.93%
and 51.93%, respectively. Its maximum overshoot was worse: 13,428 mC versus
6,840 mC. These same-clock QEMU values are observed sample maxima and
percentiles, not hardware real-time bounds.

The separate cross-guest ACK-loss run completed 100/100 commands and has source
log SHA-256
`f15c88c6671db67934ce178e3f113b65ac2811a1538a0c36412f6c156bd279fd`.
It contains exactly 20 first-ACK suppressions at sequences 5 through 100 in
steps of five, 20 retransmissions/recoveries and duplicates, 100 fresh
applications, 120 STATUS frames, 100 ACK frames, and zero ERROR frames,
protocol errors, or terminal timeouts. Full-loop p50/p95/p99/maximum was
3,953/110,808/111,484/111,548 us; the intentional retry delay dominates the
tail.

The compressed raw logs, analyzer summaries, QEMU exits, source snapshot,
configuration hashes, rootfs/controller hashes, and both normal/fault Zephyr
image hashes are retained under
[`results/axvisor-ivc-reference`](results/axvisor-ivc-reference/).

## 9. Extension points and residual assurance limits

- A port can be placed in another `u16` segment without changing the switch;
  it will be isolated from segment 1.
- Message types and typed errors permit future telemetry without changing the
  fixed header.
- The `no_std` Rust protocol library can be reused by another RTOS port.
- More controller policies can implement the same observation-to-command
  boundary and retain the on-wire contract.

The required shared/partitioned idle/stress/soak campaign, deterministic
cross-guest ACK-loss campaign, and durable QEMU evidence are complete. The
actual approximately five-minute video and intentionally deferred dev-target
PR remain outstanding.

Cross-guest malformed-ERROR, controller-restart, and a third-guest runtime
cross-segment negative capture would strengthen the evidence, but the current
ERROR behavior is covered by Rust/C tests, restart by a host reference, and
access control by the no-external-NIC topology plus switch policy regressions.
Those scopes are labeled honestly in the test report. Low-overhead evidence is
still required before claiming bounded guest preemption or direct IRQ latency.
The native Zephyr baseline is an explicitly different QEMU platform and is
suitable only for the qualified comparison documented with its results.
