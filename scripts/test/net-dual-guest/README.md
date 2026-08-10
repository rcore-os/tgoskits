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
