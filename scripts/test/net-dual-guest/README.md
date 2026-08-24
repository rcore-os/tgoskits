# Linux–RTOS UDP/IP validation

This directory contains reproducible build, boot, packet-capture, protocol,
and fault-injection tools for the bidirectional Linux–RTOS network link.
The application data path is UDP/IP over dedicated VirtIO-MMIO endpoints.
Shared memory, HyperCall, raw MMIO, and vsock are not used as the main data
channel. QMP is used only to control test faults and to terminate QEMU.

## Build the endpoints

Build the static Linux endpoint and inspect the generated manifest:

```bash
scripts/test/net-dual-guest/build-linux-task2.sh
cat tmp/net-dual-guest/linux-task2/manifest.toml
```

Build the ArceOS endpoint with:

```bash
cargo xtask arceos build \
  --package arceos-task2-net \
  --arch aarch64 \
  --config apps/arceos/task2-net/build-aarch64-p1.toml
```

Build the tool dependencies and validate the test manifest:

```bash
bash scripts/test/net-dual-guest/build-tools.sh
python3 scripts/test/net-dual-guest/validate_manifest.py \
  scripts/test/net-dual-guest/manifest.toml
```

## Network topology

The two guests use independent VirtIO-MMIO endpoints and a point-to-point QEMU
socket connection. The network configuration is:

| Item | Linux guest | RTOS guest |
|---|---|---|
| VirtIO-MMIO endpoint | `/virtio_mmio@a003e00` | `/virtio_mmio@a003c00` |
| MAC address | `52:54:00:12:34:01` | `52:54:00:12:34:02` |
| IPv4 address | `10.0.42.15/24` | `10.0.42.2/24` |
| UDP service | `4242` | `4242` |

There is no bridge, NAT, host forwarding rule, or additional firewall rule.
Each endpoint binds only UDP port `4242` and validates the peer address, session
identity, frame fields, and CRC32. The manifest and runtime verifiers check the
disjoint MMIO devices, GPA/HPA mappings, DMA ranges, and IRQ routes.

## Protocol checks

The `task2-net-protocol` crate defines the versioned `T2N1` wire format. Each
datagram contains a fixed 28-byte header with the magic, version, message type,
flags, session ID, sequence number, acknowledgement number, payload length,
error code, and CRC32, followed by a payload of at most 1200 bytes.

The protocol supports `CONTROL`, `STATUS`, `ACK`, `ERROR`, and `HEARTBEAT`.
`CONTROL` and `STATUS` use ACK, timeout, bounded retransmission, duplicate
suppression, and out-of-order detection. Heartbeat timeout enters the safe
state; a valid heartbeat restores the active state. Invalid parameters and
protocol violations produce explicit `ERROR` messages.

Run the protocol unit tests and lint checks with:

```bash
cargo test -p task2-net-protocol
cargo clippy -p task2-net-protocol --all-targets -- -D warnings
```

The normal repository CI gate runs the hardware-independent protocol and
controller contract checks, including the `baseline`/`cnn`/`yolo` model modes:

```bash
bash scripts/test/net-dual-guest/run-ci-regression.sh
```

This gate does not substitute for the explicit AArch64 dual-Guest QEMU run;
the latter remains the source of packet-capture, isolation, and fault-recovery
evidence.

## Packet and isolation verification

Verify both packet captures and the application protocol:

```bash
python3 scripts/test/net-dual-guest/verify_pcap.py \
  tmp/net-dual-guest/linux.pcap tmp/net-dual-guest/rtos.pcap \
  --port 4242 --require-task2
```

Verify the device and memory isolation evidence:

```bash
python3 scripts/test/net-dual-guest/verify_fdt_devices.py \
  <host.dtb> scripts/test/net-dual-guest/manifest.toml
python3 scripts/test/net-dual-guest/verify_isolation.py \
  <axvisor.log> scripts/test/net-dual-guest/manifest.toml
```

The packet verifier compares the direction, sequence ledger, message kinds,
and ACK coverage in both captures. A successful run must show bidirectional
UDP traffic and matching ledgers; a successful QMP command alone is not
sufficient evidence of a recovered data link.

## Fault injection

Drop one ACK on the real guest link to verify retransmission and duplicate
handling:

```bash
python3 scripts/test/net-dual-guest/ack_drop_proxy.py \
  --linux-port 12731 --rtos-port 12732
```

Control the QEMU data link through its QMP UNIX socket:

```bash
python3 scripts/test/net-dual-guest/qmp_link.py \
  <qmp.sock> net-rtos off
python3 scripts/test/net-dual-guest/qmp_link.py \
  <qmp.sock> net-rtos on
```

Use `verify_fault_pcap.py` and the guest logs to confirm retransmission,
safe-state entry, and recovery. Protocol injection tests additionally verify
CRC errors, invalid parameters, duplicate frames, and out-of-order frames.

## In-hypervisor virtio-net switch

The dual-guest link runs over Axvisor's internal L2 switch
(`[[devices.virtual]] model = "virtio-net"`). Each guest drives a
hypervisor-emulated virtio-mmio endpoint at `0x0b00_0000` (guest IRQ 32,
matching AxVisor's first automatic AArch64 virtio window); frames are forwarded between
the two ports inside the hypervisor, so no QEMU netdev, socket pair, or
host-DTB carveout exists in the data path.

| Item | Linux guest | RTOS guest |
|---|---|---|
| Endpoint | virtual `virtio_mmio@b000000` (IRQ 32) | virtual `virtio_mmio@b000000` (IRQ 32) |
| MAC address | `52:54:00:12:34:01` | `52:54:00:12:34:02` |
| IPv4 address | `10.0.42.15/24` | `10.0.42.2/24` |
| UDP service | `4242` | `4242` |

The Zephyr guest selects QEMU's `virtio_mmio0` slot via
`zephyr-task2/app.overlay.switch`, which is the same base address and GIC SPI
(16) the hypervisor's generated FDT publishes. Build it with
`TASK2_ZEPHYR_VIRTIO_SLOT=0`; this selects AxVisor's first AArch64 automatic
virtio window (`0x0b000000`, guest IRQ 32). The default (`app.overlay`, slot 30) serves the
QEMU socket-pair topology.

The second RTOS endpoint uses RT-Thread commit
`6ea682795bdbac59d3700b21e159ccaa3f7632cb`, the
`qemu-virt64-aarch64` BSP, and `aarch64-none-elf-gcc` 10.2.1. It keeps the
same T2N1 state machine and changes only the socket, clock, and logging APIs.
Build the three evidence variants with:

```bash
OUT_DIR=tmp/net-dual-guest/rtthread-task2-starry-normal \
  scripts/test/net-dual-guest/build-rtthread-task2.sh
TASK2_FAULT_MODE=drop-ack-once \
  OUT_DIR=tmp/net-dual-guest/rtthread-task2-starry-drop-ack \
  scripts/test/net-dual-guest/build-rtthread-task2.sh
TASK2_FAULT_MODE=drop-ack-always \
  OUT_DIR=tmp/net-dual-guest/rtthread-task2-starry-retry-exhausted \
  scripts/test/net-dual-guest/build-rtthread-task2.sh
```

Run the required virtual scenarios with:

```bash
scripts/test/net-dual-guest/run-starry-rtthread-task23-scenario.sh normal <output-dir>
scripts/test/net-dual-guest/run-starry-rtthread-task23-scenario.sh drop-ack <output-dir>
scripts/test/net-dual-guest/run-starry-rtthread-task23-scenario.sh retry-exhausted <output-dir>
scripts/test/net-dual-guest/run-starry-rtthread-task23-scenario.sh blackout <output-dir>
```

These runs cover virtual integration only; physical-board evidence is a
separate follow-up.

Run one closed-loop experiment (driver-controlled lifecycle: boot, capture,
fault, pcap streaming, QMP quit):

```bash
bash scripts/task3/run-task3-switch.sh <label> ai          # or: baseline
bash scripts/task3/run-task3-switch-fault.sh <label>       # blackout 25s..35s
```

The console driver (`serial_console.py`) owns the QEMU serial socket, executes
the step script, and streams `virtnet capture dump` output back into per-VM
pcap files (`switch.vm1.pcap` / `switch.vm2.pcap`). Hypervisor-side control
commands:

```text
virtnet show                    switch state, port table, blackout/capture flags
virtnet drop on|off             drop every frame in both directions (blackout)
virtnet capture on|off          enable/disable per-frame capture at port boundary
virtnet capture dump [PATH]     stream frames to the console (pcap), or write files
```

The same T2N1 frame ledger must appear in both captures; verify with:

```bash
python3 scripts/test/net-dual-guest/verify_pcap.py \
  results/task3/switch/<label>/linux.pcap \
  results/task3/switch/<label>/rtos.pcap \
  --port 4242 --require-task2 --min-ack-rate 80
```

Evidence is archived per run under `results/task3/switch/<label>/` with
`run.log`, `build.log`, both pcaps, and a run manifest. Known characteristics
of the polling-based RX delivery: the median control period is ~130 ms with
periodic ~300 ms spikes tied to heartbeat/wakeup races in the vCPU notify
path; this is documented in the Task-3 design document rather than hidden.
