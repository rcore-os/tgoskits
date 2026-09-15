# AICP v1 protocol

AICP (AI Control Protocol) carries AI-derived control commands from an
application Guest to a control Guest over an IP network. It defines the
fixed-layout request/reply contract used by the QEMU demonstration.

## Protocol scope

v1 defines a bounded request/response control path whose C and Rust
implementations exchange byte-identical frames. Both TCP and UDP are supported
application transports. Their request/reply semantics are defined in the
[project design](../../docs/design/aicp-control-protocol.md).

## Scope and alternatives

The repository already has guest IPC mechanisms, but they are not the IP data
path required by this demonstration. DDS/RTPS, MQTT, OPC UA and gRPC provide
broader discovery, data modelling, security or RPC facilities; their runtime
and deployment cost is outside this bounded Guest-to-Guest control example.
AICP provides TCP as the default ordered control transport and UDP as a
parallel datagram transport. TCP supplies ordered delivery and reconnection.
For UDP, a matching `STATUS` or `ERROR` is the application acknowledgement; an
initiator retries a timeout by resending the same sequence and the server
replays the cached result. This keeps duplicate control requests idempotent
without adding another acknowledgement frame.

The shared wire definition and validation rules live in this crate. The C
reference codec mirrors them under `apps/ai-rtos-demo/aicp/`; guest network
setup and control-output ownership remain in their respective applications.
The complete ownership, sequence, resource, compatibility, and validation
decisions are in the [AICP project design](../../docs/design/aicp-control-protocol.md).

### Relationship to active guest-network work

This crate is the sole application protocol selected for the AICP contribution
tracked by Issue #2154; it is not a second general-purpose Guest control
protocol. Its Linux/Starry clients, RTOS services, model loop, and dual-Guest
runner use the same AICP v1 contract over TCP or UDP. The following active work
is not selected for that contribution and must not be mixed on one Guest link:

| Work | Boundary | Relationship to AICP |
| --- | --- | --- |
| #2156 | Generic TCP Guest control protocol | Not selected. Its TCP framing is incompatible with AICP and it is neither an AICP dependency nor an adapter target. |
| #1958 / #1971 | T2N1 reliable UDP transport and its AI loop | Not selected as a protocol dependency. This does not disable AICP UDP: AICP uses its own `STATUS`/`ERROR` replay for bounded datagram request/reply instead of T2N1's session/ACK contract. |
| #1697 | VirtIO-net capability and Guest network configuration | Required transport prerequisite. It owns NIC selection, addressing, and the AxVisor network configuration; AICP owns no competing network setup. |

The ownership boundary is deliberately narrow: this crate and its mirrored C
codec own AICP frame validation; application Guests own control state and
deployment; AxVisor and #1697's network work own the virtual NIC path. The
#2154 integration does not provide a fallback to #2156 or T2N1 and does not
maintain a permanent adapter layer. A future repository-wide replacement must
migrate AICP callers as one unit rather than silently accepting another wire
contract.

## Wire format

Every frame is a 32-byte big-endian header followed by `payload_len` bytes:

| Bytes | Field |
| --- | --- |
| 0..2 | magic `0xa1c0` |
| 2 | version (`1`) |
| 3 | message type |
| 4..6 | flags, must be zero in v1 |
| 6..8 | header length (`32`) |
| 8..12 | payload length (at most 4096) |
| 12..16 | sequence number |
| 16..24 | sender timestamp in ns |
| 24..26 | error code |
| 26..28 | CRC-16/CCITT over header with this field zero and payload |
| 28..32 | reserved, must be zero in v1 |

`CONTROL_SET` uses five IEEE-754 binary32 values and a `u32` mode, each in
network byte order. `STATUS` uses four binary32 values and two `u32` fields.
The C and Rust encoders use these byte layouts, never native struct layout.
Before a control state update, all floating values must be finite; target and
feed-forward are in `[-1, 1]`, gains are in `[0, 10]`, and mode is `0` or `1`.

## State and compatibility

`HELLO`, `HEARTBEAT`, and `CONTROL_SET` receive `STATUS`; malformed or
unsupported requests receive `ERROR`. A connection consumes each accepted
sequence number and replays the cached result for an exact duplicate; older
numbers receive `ERROR_SEQUENCE`. A new TCP connection begins a new sequence
window while sharing only the control state. Unknown versions are read and
CRC-checked by a server before it returns `ERROR_VERSION`; clients reject them.
Unknown flags or non-zero reserved bytes are rejected in v1. A future
incompatible extension requires a new version rather than setting v1 bits.

TCP owns ordered delivery and connection recovery. The TCP connection session
owns its sequence window and cached reply; the server's control state is shared
only for the observed control output. UDP owns a separate bounded peer cache.
When that cache is full, a new peer receives `ERROR_INTERNAL`; an existing
peer's replay entry is never evicted merely to admit it. A UDP peer remains
valid for a 30-second replay window; after expiry the endpoint must send
`HELLO` before a `CONTROL_SET` can be accepted. The source-address-only UDP
path is not an authentication mechanism.

## Compatibility vectors and validation

The C and Rust implementations share fixed header, CRC and control-payload
vectors. `make -C apps/ai-rtos-demo test` exercises C encoding, invalid header
options, malformed control input, version handling and reconnect behavior.
`cargo test --manifest-path components/aicp-protocol/Cargo.toml --all-features`
exercises the corresponding Rust byte vectors and validation rules. The
ArceOS server tests additionally cover unknown-version error responses,
duplicate replay and a new TCP connection beginning at sequence `1`.

The deployment assumes the configured virtual network is trusted. AICP v1
does not authenticate or encrypt traffic; deployment isolation is provided by
the Guest network topology. Rolling back removes the AICP application and
restores the prior Guest image/configuration; it does not alter the AxVisor
wire interfaces.
