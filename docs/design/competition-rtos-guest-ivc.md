# RT-Thread and FreeRTOS guest IVC design

Status: implemented and validated on QEMU/AArch64

Scope: QEMU/AArch64 AxVisor guests under `competition/ivc/`

## Problem and users

Before this change, the competition path proved the IVC/1 control loop only
with a Zephyr guest, while the RT-Thread and FreeRTOS additions proved native
scheduling baselines. Existing RTOS VM TOML files did not provide reproducible images,
virtual NIC integration, an IP endpoint, or end-to-end evidence. They therefore
cannot be counted as guest or IVC support.

The users are competition reviewers reproducing cross-guest communication and
maintainers comparing the same controller against more than one RTOS without
changing the wire protocol or AxVisor device model.

## Success criteria

Each new RTOS path must:

1. verify and stage a source-pinned AArch64 image rather than accept an opaque
   prebuilt binary;
2. boot as an AxVisor virtualized guest and emit an RTOS-specific ready marker;
3. negotiate the existing VirtIO 1.x MMIO network device at guest IPA
   `0x0b000000`, validate MAC `52:54:00:00:00:02`, and use switch segment 1;
4. configure IPv4 `10.0.0.2/24` and bind UDP `10.0.0.2:5500`;
5. pass the existing IVC/1 golden vector and use the same protocol and endpoint
   state machines as Zephyr;
6. complete a bounded Linux-controller normal campaign with exact STATUS then
   ACK responses, no protocol errors, and accepted count equal to the requested
   count;
7. complete deterministic ACK-loss coverage in which retransmission does not
   reapply a command; and
8. retain source identity, build command, artifact hashes, raw console, and a
   machine-checked summary.

The existing native RTOS measurements remain separate evidence and must not be
relabelled as guest measurements.

## Risk classification

This is high risk for the competition path because it introduces two guest
network stacks, DMA-visible queue memory, and a new external dependency. It
does not alter the public Rust workspace API, StarryOS syscall semantics, or a
physical-board contract. The first supported platform is QEMU/AArch64 only;
cache-maintenance and device-address assumptions for physical boards are
explicitly out of scope.

## Chosen boundaries

### Shared wire and endpoint core

The existing Apache-2.0 C IVC/1 codec and endpoint state machine are the source
of truth for all three RTOS guests. RTOS adapters own only clocks, UDP sockets,
network startup, and console output. The shared core retains the 32-byte
little-endian header, CRC32, bounded receive window, duplicate handling,
timeout-safe state, and thermal model already cross-checked against the Rust
`ivcproto` implementation.

### Shared VirtIO network transport

A small repository-owned VirtIO 1.x MMIO network transport is shared by the
RT-Thread and FreeRTOS adapters. It owns only the transport handshake, one RX
and one TX split queue, fixed 12-byte `virtio_net_hdr`, bounded Ethernet
buffers, and runtime MAC verification. It negotiates only
`VIRTIO_F_VERSION_1`, `VIRTIO_NET_F_MAC`, and optionally
`VIRTIO_NET_F_STATUS`; offload, mergeable buffers, indirect descriptors,
event-index, control queues, and multiqueue are rejected.

The driver uses statically allocated queue and packet memory whose CPU and guest
physical addresses are identical on the selected guest platforms. MMIO is a
separate boundary: RT-Thread maps the device GPA with `rt_ioremap`, while the
Bao FreeRTOS runtime uses its QEMU identity mapping. RX is polled by a bounded
RTOS task; TX completion is bounded and checked.
Polling avoids embedding either RTOS's interrupt and wake primitives in the
transport core and is acceptable for the 100 ms control period. The AxVisor
IRQ resource remains present in the guest device contract so a later adapter
can switch to interrupt-driven wakeup without changing MMIO or wire layout.

### RT-Thread adapter

The guest stays pinned to RT-Thread v5.2.2 and its
`qemu-virt64-aarch64` BSP. The build enables the in-tree lwIP socket layer,
but does not enable the BSP's legacy VirtIO driver: that driver probes QEMU's
version-1 transport at `0x0a000000` and programs `QUEUE_PFN`, which is
incompatible with AxVisor's version-2 transport. A small `eth_device` adapter
connects the shared raw Ethernet driver to lwIP and a socket adapter connects
the lwIP UDP API to the shared IVC server. SAL remains disabled because it is
not needed by this application.

### FreeRTOS adapter

The guest stays on the pinned `freertos-over-bao` AArch64 runtime and kernel.
It adds official FreeRTOS-Plus-TCP V4.4.1 at tag commit
`c12361095aca68aeed858f45d14395fbffa92c0d`. Only IPv4, ARP, UDP, and the
socket layer are enabled; DHCP, DNS, IPv6, TCP, and IP fragmentation are out of
scope. A FreeRTOS-Plus-TCP `NetworkInterface_t` adapter connects network
buffers to the same raw VirtIO driver.

## Guest resource contract

Both images use one vCPU and a private 128 MiB RAM region. Their linked RAM
bases remain OS-specific: RT-Thread uses `0x40000000`, while the Bao FreeRTOS
platform uses `0x50000000`. Neither guest receives a host device-tree or
passthrough device. The first dynamic virtual-device slot resolves as follows:

| Resource | Guest value |
| --- | --- |
| VirtIO MMIO base / size | `0x0b000000` / `0x1000` |
| Architectural interrupt ID | 32 |
| MAC | `52:54:00:00:00:02` |
| Header layout | fixed 12-byte |
| Switch segment | 1 |
| IPv4 / UDP | `10.0.0.2/24`, port 5500 |

The Linux controller remains VM 1 at `10.0.0.1` with MAC suffix 1. An RTOS
endpoint is VM 2 with MAC suffix 2. RT-Thread and FreeRTOS are alternate
campaign endpoints, not simultaneous owners of the same identity.

## Failure and safety behavior

- Probe fails closed on wrong magic, MMIO version, device ID, required feature,
  queue size, status transition, or MAC.
- Queue lengths and Ethernet frames are bounded; descriptor indices and used
  lengths are validated before copying.
- TX timeout or malformed RX increments an explicit error counter and cannot be
  reported as a successful IVC campaign.
- IVC malformed frames return the existing typed ERROR response when enough
  request context is valid; duplicates return STATUS and ACK without applying
  control again.
- Silence beyond 500 ms enters the existing safe state with actuator zero.
- Build scripts reject source drift and dirty third-party checkouts and refuse
  to overwrite evidence directories.

## Alternatives considered

### Patch RT-Thread's legacy VirtIO driver

Rejected as the primary boundary. Updating its probe, feature selectors,
modern queue addresses, status sequencing, and hard-coded base/IRQ would fork
an RT-Thread-wide driver for one competition guest and would not help
FreeRTOS. The shared narrow transport makes the compatibility surface explicit.

### Add legacy VirtIO MMIO emulation to AxVisor

Rejected. It would expand a hypervisor-wide device contract to preserve an old
guest shortcut, while all existing IVC guests already use VirtIO 1.x. Guest
adaptation is smaller and keeps one transport generation in AxVisor.

### Reuse RT-Thread's lwIP sources in FreeRTOS

Rejected. Those sources are part of the RT-Thread tree and their OS port is not
a FreeRTOS capability boundary. Official FreeRTOS-Plus-TCP has the native task,
buffer, and socket integration needed by the FreeRTOS guest.

### Implement a custom ARP/IPv4/UDP stack

Rejected. Although the demo needs only UDP, owning packet parsing, checksums,
ARP aging, and socket semantics would add more security-sensitive protocol code
than the driver itself and would provide weaker upstream validation.

### Treat boot markers or host loopback as IVC completion

Rejected. Guest support requires the real AxVisor virtual device and the Linux
controller on the internal L2 segment. Host tests remain useful only as lower
level regression coverage.

## Validation plan

1. Host tests for the shared IVC golden vector, exact-once receive window,
   timeout fallback, and ACK-loss policy.
2. Host tests with a fake MMIO device for feature negotiation, queue layout,
   wrong identity/features, oversized frames, and corrupt used entries.
3. RT-Thread and FreeRTOS guest builds from verified source pins, with inherited
   upstream/compiler warnings retained in the build log.
4. ELF entry/load-address checks and bounded AxVisor boot-ready tests for each
   guest.
5. Linux-controller normal and deterministic ACK-loss QEMU campaigns for each
   RTOS, followed by strict log analysis and artifact hashing.
6. Existing Zephyr IVC host/QEMU contracts, AxVisor device tests, formatting,
   and applicable Rust clippy checks remain green.

## Validation result

On 2026-08-15 all four Linux-controller/RTOS QEMU campaigns passed strict
analysis:

| RTOS / profile | RTOS result | Controller result | QEMU log SHA-256 |
| --- | --- | --- | --- |
| RT-Thread normal | accepted/applied 100/100, duplicate 0, protocol error 0 | acknowledged 100, retransmission 0 | `2a6199ea0ff6e1d617fd0f70edaca72e70996b0311ec986a1bf742cfc6ddb926` |
| RT-Thread ACK-loss | accepted/applied 100/100, duplicate/drop 20/20 | acknowledged 100, retransmission/recovery 20/20 | `19c3a64091ba2e0a9c51973ff0cd318144a2cc8e56f7353d87a973800720433d` |
| FreeRTOS normal | accepted/applied 100/100, duplicate 0, protocol error 0 | acknowledged 100, retransmission 0 | `abb9489b672121946905f0a93e10adf06375d9dae0aac1a61ea1ed8c2b5c25b1` |
| FreeRTOS ACK-loss | accepted/applied 100/100, duplicate/drop 20/20 | acknowledged 100, retransmission/recovery 20/20 | `3a669979e73cd6672641c4e9543cba37293203300f827f3d4f2e7d6a30305243` |

The first RT-Thread run exposed a deterministic translation fault at the raw
MMIO GPA after the BSP enabled its MMU. A regression contract now requires the
RT-Thread adapter to call `rt_ioremap` and fail closed before the shared driver
touches registers. Rebuilt normal and ACK-loss images then passed the same ELF,
boot, network, protocol, and evidence gates as FreeRTOS.

These are QEMU/AArch64 endpoint results, not RK3588 board results or real-time
latency claims. The normal campaign may enter the existing safe state after its
terminal result when the controller stops sending; the ACK-loss campaign may do
so during each deliberate 500 ms transport gap. In both cases the exact-once
counter remains `applied=100`.

## Rollback

The change is additive. Removing the two RTOS IVC application directories,
their alternate VM/board configurations, and the shared raw driver restores the
Zephyr-only path. The IVC/1 wire format and persisted evidence schemas are not
migrated.
