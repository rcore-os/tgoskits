# net_stats - Network Statistics eBPF Monitor

Real-time network statistics monitoring for StarryOS using eBPF kprobes.

## Features

- **TX/RX packet and byte counters**: accurate counts for all IP traffic
- **Consistent with `/proc/net/dev`**: probes the exact same counting points
  (`DeviceHandle::count_tx` / `DeviceHandle::count_rx`) that maintain the
  kernel's own rx_packets/rx_bytes/tx_packets/tx_bytes counters
- **Low overhead**: eBPF-based probing with per-CPU maps, no lock contention
- **Cross-architecture**: supports x86_64, aarch64, riscv64, loongarch64
- **Protocol-agnostic**: counts all IP frames at the physical layer (TCP, UDP, ICMP, etc.)

## Usage

### Interactive Mode (default)

Monitor network statistics in real-time with periodic updates:

```bash
/usr/bin/net_stats
```

Press Ctrl+C to exit.

### Test Mode

Run validation tests to ensure probes are working:

```bash
/usr/bin/net_stats --test
```

### Single Snapshot

Print current statistics once and exit:

```bash
/usr/bin/net_stats --once
```

## Output Format

```
NET_STATS_BEGIN
tx_pkts=10  tx_bytes=640
rx_pkts=12  rx_bytes=768
NET_STATS_END
```

- `tx_pkts`: number of frames transmitted
- `tx_bytes`: total bytes transmitted (L2 frame length)
- `rx_pkts`: number of frames received
- `rx_bytes`: total bytes received (L2 frame length)

**Note**: Byte counts represent L2 frame sizes (including protocol headers and
per-device L2 framing overhead, excluding trailing FCS), aligned with Linux
`/proc/net/dev` semantics.

## Implementation Details

### Probe Points

The eBPF program attaches kprobes at the exact functions where ax-net updates
its `/proc/net/dev` counters:

- `DeviceHandle::count_tx(&self, len: usize)` → TX_PKTS +1, TX_BYTES +len
- `DeviceHandle::count_rx(&self, len: usize)` → RX_PKTS +1, RX_BYTES +len

Both functions carry `#[inline(never)]` in `net/ax-net/src/router.rs` so they
survive release-mode inlining and remain attachable by kprobe.

On x86_64, `&self` is in `rdi` (arg 0) and `len` is in `rsi` (arg 1). The
eBPF probe reads `len` from arg 1 for both TX and RX — simple, symmetric, and
ABI-stable.

### Why This Approach

Earlier implementations probed socket-layer methods and read byte counts from
return values (`AxResult<usize>` via sret), then switched to probing
`TxToken::consume` / `RxToken::consume` at the smoltcp phy layer. Both had
fundamental issues:

1. **Re-implements counting**: the eBPF independently counted packets/bytes
   instead of observing the kernel's own authoritative counters, leading to
   divergence.
2. **RX bytes not available at `RxToken::consume`**: the function signature
   `RxToken::consume(self, f: F)` has no `len` argument, and reading the
   slice length from the inlined struct required guessing memory offsets.

By probing `count_rx` / `count_tx` directly:

- **Observes the authoritative counters**: every call to these functions
  corresponds to a `fetch_add` on the `AtomicU64` fields that back
  `/proc/net/dev`. The eBPF counters stay naturally consistent.
- **RX and TX are symmetric**: both take `(&self, len: usize)`, so byte
  counting works the same way for both directions.
- **Simple, non-generic signatures**: no monomorphized variants — each
  function resolves to exactly one symbol.

### Symbol Resolution

Symbols are resolved dynamically from `/proc/kallsyms` using Rust v0 name
mangling fragments:

- TX: `["6ax_net", "6router", "12DeviceHandle", "8count_tx"]`
- RX: `["6ax_net", "6router", "12DeviceHandle", "8count_rx"]`

Each resolves to exactly one symbol (no monomorphized variants — the methods
are concrete, not generic).

### Per-CPU Maps

Counters use `BPF_MAP_TYPE_PERCPU_ARRAY`: each CPU writes to its own private
slot, eliminating cache-line contention and the need for atomic operations.
The userspace loader aggregates by summing across all CPU slots when printing
or testing.

## Building

The eBPF program is built automatically as part of the StarryOS build:

```bash
cargo xtask starry app qemu --test-case ebpf/net_stats --arch x86_64
```

Manual build:

```bash
cd apps/starry/ebpf/net_stats
./prebuild.sh  # Builds the eBPF object
cargo build --release  # Builds the userspace loader
```

## Testing

Run automated validation:

```bash
cargo xtask starry app qemu --test-case ebpf/net_stats --arch x86_64
```

The test performs TCP and UDP loopback operations and verifies that all four
counters (tx_pkts, tx_bytes, rx_pkts, rx_bytes) are non-zero.

## Troubleshooting

### "no symbols found in /proc/kallsyms"

Verify the `#[inline(never)]`-annotated functions exist:

```bash
grep "12DeviceHandle" /proc/kallsyms | grep "8count_tx"
grep "12DeviceHandle" /proc/kallsyms | grep "8count_rx"
```

If symbols are missing, check that the running kernel was built with the
`#[inline(never)]` annotation on `DeviceHandle::count_tx` and
`DeviceHandle::count_rx` in `net/ax-net/src/router.rs`.

### All counters are zero

1. Check that network operations are actually occurring
2. Verify eBPF programs loaded: check dmesg for load errors
3. Check for eBPF verifier errors in dmesg

### eBPF logger warning

```
[WARN] failed to initialize eBPF logger: AYA_LOGS not found
```

This is expected — the aya logging map is optional and does not affect
functionality.

## License

Dual MIT/GPL (required for eBPF programs)
