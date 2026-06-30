# IVC application-layer protocol (Task 2)

A real application-layer protocol over **UDP/IP** for inter-guest communication,
plus a client/server automated test harness. Runs in the Linux guests on top of
the emulated virtio-net device and the AxVisor software L2 switch.

This is **not** ping/iperf/nc/raw-socket: it defines its own versioned framing,
message types, sequencing, integrity check, and reliability layer, and reports
structured results.

## Wire format (16-byte header, little-endian)

| field        | size | notes                                             |
|--------------|------|---------------------------------------------------|
| magic        | u16  | `0xA1B2`                                           |
| version      | u8   | `1`                                                |
| msg_type     | u8   | `1`=DATA `2`=ACK `3`=CONTROL `4`=STATUS `5`=ERROR  |
| seq          | u32  | per-message sequence number                        |
| timestamp_ms | u32  | sender timestamp (RTT / freshness)                 |
| payload_len  | u16  | payload byte count                                 |
| checksum     | u16  | ones'-complement sum over header(ck=0)+payload     |

Covers the required header fields: **version, message-type, payload-length,
sequence-and-timestamp, checksum**, and the control/status/error message classes.

## Reliability (UDP)

- **ACK + timeout + retransmit**: the client sends `DATA(seq)` and waits for
  `ACK(seq)` with a 400 ms timeout, retransmitting up to 6 times.
- **De-duplication / reorder tolerance**: the server tracks each `seq` and counts
  a repeat as a duplicate (not a new delivery), so retransmits and reordered
  packets are handled correctly.
- **Integrity**: the server validates the checksum and replies `ERROR` on a
  mismatch (counted as `corrupt`).
- **Control handshake**: the client first exchanges `CONTROL`/`STATUS` (with its
  own retransmit) to confirm the server is reachable before the data phase.
- **Lossy mode** (`server ... lossy=K`): the server deliberately drops the first
  `ACK` for every Kth distinct seq, to *force* the client's retransmit path and
  the server's dedup path to execute during the automated test.

## Roles

```
ivcproto server <bind_addr> [lossy=K]
ivcproto client <peer_addr> <count>
```

`guest-init.sh` (PID 1) derives the IP from the virtio-net MAC
(`52:54:00:00:00:01` -> `10.0.0.1` = server, `..:02` -> `10.0.0.2` = client),
brings up `eth0`, and runs the matching role. Output lines are prefixed `PROTO-`
for the harness to scrape.

## Validation

The protocol logic was validated end-to-end on a host loopback with the lossy
server forcing the reliability paths:

```
PROTO-CLIENT-RESULT sent=40 acked=40 lost=0 retransmits=8 rtt_avg_ms=81.49 ...
PROTO-SERVER-RESULT unique=40 dups=8 corrupt=0 acks_dropped=8
```

Internally consistent: 8 dropped ACKs -> 8 client retransmits -> 8 server-side
de-duplicated repeats -> all 40 messages delivered exactly once (0 lost, 0
corrupt). This exercises versioned framing, all message types, sequencing,
checksums, ACK/timeout/retransmit, and dedup/reorder tolerance.

In-guest, the server role has been confirmed running on the real network stack
(`PROTO-SERVER listening on 0.0.0.0:5500 lossy=5`) after the guest brings up the
emulated `eth0`. Running the full client<->server exchange across **two**
simultaneous guests is currently gated only by AxVisor's unreliable
simultaneous 2-VM boot (cooperative non-preemptive scheduler; see
`../M5-network-design.md`), not by the protocol or the network path — the
underlying bidirectional link itself is proven (see the captured ICMP run).

## Build

`ivcproto` is a standalone static `aarch64-unknown-linux-musl` binary (its
`Cargo.toml` carries an empty `[workspace]` so it is independent of the monorepo
workspace):

```
cargo build --release --target aarch64-unknown-linux-musl
```

`build-rootfs.sh` assembles a minimal ext4 guest rootfs (static busybox + the
musl loader + `ivcproto` + `guest-init.sh`) with `mkfs.ext4 -d`, which both
avoids debugfs directory-index issues and boots fast.
