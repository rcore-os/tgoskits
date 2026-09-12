# AICP Guest Control Protocol Design

## Problem and scope

AxVisor Guest demonstrations need an IP application protocol through which an
application Guest can submit a control command and receive the control Guest's
observed status. Existing HyperCall, shared-memory, and vsock mechanisms are
useful for VM management and debugging but do not provide the required IP data
path. AICP (AI Control Protocol) defines that bounded data-plane contract for
the demonstration tracked by [Issue #2154](https://github.com/rcore-os/tgoskits/issues/2154).

The direct users are C Linux/RTOS applications and the ArceOS control service.
Success means that C and Rust endpoints exchange byte-identical frames, a
control Guest either returns `STATUS` or an explicit `ERROR`, and a retry never
applies the same control command twice. AICP is a high-risk shared protocol:
its frame layout, sequence handling, and resource limits are public behavior.

## Selected protocol route and boundary

| Option | Decision |
| --- | --- |
| HyperCall, shared memory, or vsock | Not the primary path: they do not exercise the required IP network boundary. |
| #2156 Guest TCP protocol | Not selected for the #2154 control path. It has a different wire contract and is not an AICP dependency or adapter target. |
| #1958/#1971 T2N1 UDP protocol and AI loop | Not selected as a protocol dependency for the #2154 control path. This does not disable AICP UDP: AICP keeps its own datagram replay semantics and does not mix T2N1's session/ACK contract. |
| #1697 Guest VirtIO-net capability | Required below AICP. It owns the IP-capable NIC/configuration boundary, not control framing. |
| AICP | The sole application protocol selected for the #2154 control path. It supports both TCP and UDP over the same v1 frame contract. |

This selection is intentionally about the #2154 AICP contribution: Linux/Starry
clients, RTOS services, the model loop, and the dual-Guest runner in that work
use AICP only. There is no permanent protocol adapter and no mixed link on
which an endpoint may send AICP and receive #2156 or T2N1 traffic. The other
open proposals retain their own review scope, but they are not prerequisites,
fallbacks, or alternate implementations of this contribution. If maintainers
later select a different repository-wide control protocol, the AICP callers
must migrate as one unit; silently supporting several incompatible contracts
is not an option.

AxVisor and VirtIO-net own NIC provisioning, addressing, routing, and network
isolation. AICP owns only application framing, validation, sequence state, and
request outcomes. The control application owns the control-state update; the
application Guest owns model inference and command generation.

## Data flow and ownership

```text
application Guest                 control Guest
model/policy -> AICP frame --IP--> validate -> sequence decision -> control state
                       <--IP-- STATUS or ERROR
```

The shared Rust crate `aicp-rust-protocol` is the normative AICP v1 schema and
validation implementation. The C codec under `apps/ai-rtos-demo/aicp/` mirrors
that schema because C Guests cannot link the Rust crate. Cross-language vectors
and tests are the conformance boundary; axbuild and Guest runners consume the
protocol but do not define a second wire schema.

`ControlState` is shared by a service. TCP sequence/reply state belongs to one
accepted connection. UDP sequence/reply state belongs to one source endpoint
for its replay lifetime. Thus reconnecting TCP clients begin a new sequence
window without resetting the observed control state, while duplicate UDP
datagrams are answered from the endpoint's cache.

## Wire contract and versioning

Every AICP v1 frame has a 32-byte big-endian header, at most 4096 payload bytes,
and CRC-16/CCITT computed over the header with its CRC field zeroed plus the
payload. `CONTROL_SET` and `STATUS` are fixed 24-byte payloads made of
network-order IEEE-754 binary32 values and `u32` fields; native C or Rust struct
layout is never sent on the wire.

Version 1 requires `flags == 0` and `reserved == 0`. Encoders reject unsupported
options before writing a frame. A server may read and CRC-check a structurally
valid unknown-version frame solely to return `ERROR_VERSION`; clients reject an
unknown-version response. There is no silent downgrade. Any incompatible
extension gets a new version and its own compatibility/migration decision.

## Transport and state semantics

TCP is the default ordered control path. It uses framed reads, an absolute
per-frame I/O deadline, explicit disconnect/reconnect handling, and one cached
reply per connection. `HELLO`, `HEARTBEAT`, and `CONTROL_SET` return `STATUS`;
invalid input returns `ERROR`. Repeating the current sequence replays the
cached outcome; an older sequence returns `ERROR_SEQUENCE`.

UDP is also an AICP v1 transport for datagram/reliability comparison. A
`STATUS` or `ERROR` having the same sequence number is the application-level
acknowledgement. A conforming initiator retries a timeout by resending the same
complete frame and sequence number; it does not set a retransmission flag.
The control service caches and replays the first outcome, rejects stale or
out-of-order requests, and therefore preserves idempotence across a retry.
The protocol has no separate `ACK` message or v1 ACK/retransmission flags.

The service limits TCP sessions and UDP peer sessions to 16. UDP peer state is
retained for 30 seconds to protect replay semantics, then reclaimed. An expired
endpoint must send `HELLO` before starting a new control sequence window; a new
endpoint is rejected while all unexpired peer slots are occupied rather than
evicting replay state.

## Trust, operations, and rollback

AICP assumes the configured Guest network is trusted. It validates frame shape,
CRC, version, sequence, finite numeric values, control ranges, and bounded
resource usage; it does not turn source addresses into authentication. Network
access control and Guest isolation remain AxVisor/network configuration duties.

Protocol health is observable through `HELLO`, `CONTROL`, duplicate/stale, and
`ERROR` logs, plus request sequence and control status. Operators disable the
feature by removing the AICP application from the Guest image or restoring the
previous Guest configuration; that rollback does not change AxVisor's VM
interfaces or a persistent data format.

## Validation matrix

| Contract | Evidence |
| --- | --- |
| C framing, CRC, version/options, reconnect | `make -C apps/ai-rtos-demo test` |
| Rust frame encoding/validation vectors | `cargo test --manifest-path components/aicp-protocol/Cargo.toml --all-features` |
| ArceOS TCP/UDP server sequence and replay state | Covered by the consuming ArceOS service PR; it is intentionally outside this protocol-core PR. |
| C reference listener and slow-frame recovery | `make -C apps/ai-rtos-demo reference-server-smoke` |
| Guest/RTOS integration | the AxVisor runner and CI attached to the consuming integration PR |

The first four commands are deterministic host checks. The last row remains a
separate integration responsibility because it owns Guest images and virtual
network topology rather than the AICP wire contract itself.
