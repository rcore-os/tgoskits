# axivc

`axivc` provides reusable shared-memory protocol helpers for AxVisor
inter-VM communication. It is a `no_std` crate intended to be used by guest
code after AxVisor has mapped the same IVC channel into more than one VM.

## Layering

`axivc` intentionally does not issue hypercalls. The IVC stack is split into
three layers:

- `axhvc`: raw guest-hypervisor ABI, including hypercall numbers, register
  argument order, architecture-specific trap instructions such as `hvc #0`,
  and low-level publish/subscribe wrappers.
- `axivc`: shared-memory protocol layout and operations after a channel is
  mapped, including `IvcRegion`, message slots, and ring-buffer send/receive.
- guest OS glue: virtual-to-physical translation for hypercall output slots,
  GPA mapping through the guest memory manager, and application policy.

This boundary keeps architecture-specific hypercall mechanics out of the
shared-memory protocol crate. A guest that wants a complete IVC flow should use
`axhvc` to publish or subscribe to a channel, map the returned GPA with its OS
memory API, and then treat the mapped region as an `axivc::IvcRegion`.

## Protocol

The current protocol is a compact single-page format:

- The first two `u64` fields match AxVisor's host-side `IVCChannelHeader`
  layout: publisher VM ID and channel key.
- `IvcRegionHeader` records magic, version, region size, feature flags, and
  ring offsets.
- Two fixed-slot single-producer/single-consumer rings are provided:
  publisher-to-subscriber and subscriber-to-publisher.
- Each slot carries message kind, sequence number, payload length, and a fixed
  payload buffer.

The ring protocol uses Release/Acquire ordering: the producer writes the slot
payload and releases `tail`; the consumer acquires `tail`, copies the slot, and
releases `head` to return ownership.

## Guest Use

A publisher typically:

1. Calls `axhvc::ivc::publish_channel`.
2. Maps the returned shared-memory GPA.
3. Initializes the mapped memory with `IvcRegion::initialize`.
4. Sends messages with `IvcRegion::send_request`.
5. Optionally receives acknowledgements with `IvcRegion::try_recv_ack`.

A subscriber typically:

1. Calls `axhvc::ivc::subscribe_channel`.
2. Maps the returned shared-memory GPA.
3. Validates `channel_header_matches` and `protocol_header_matches`.
4. Receives messages with `IvcRegion::try_recv_request`.
5. Optionally replies with `IvcRegion::send_ack`.

## Current Limits

- The region is designed to fit in the current 4 KiB AxVisor IVC channel.
- Rings are single-producer/single-consumer.
- Payload slots are fixed size.
- Notification is not part of this crate yet; current demos poll the rings.
- Access control, quotas, and channel lifecycle remain AxVisor or guest policy.
