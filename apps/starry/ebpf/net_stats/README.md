# net_stats - Network Statistics eBPF Monitor

Real-time network statistics monitoring for StarryOS using eBPF kprobes.

## Features

- **TCP statistics**: TX/RX packet and byte counters
- **UDP statistics**: TX/RX packet and byte counters  
- **Low overhead**: eBPF-based probing with minimal performance impact
- **Cross-architecture**: Supports x86_64, aarch64, riscv64, loongarch64

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
tcp_tx_pkts=10  tcp_tx_bytes=640
tcp_rx_pkts=12  tcp_rx_bytes=768
udp_tx_pkts=6   udp_tx_bytes=384
udp_rx_pkts=10  udp_rx_bytes=640
NET_STATS_END
```

- `tcp_tx_pkts`: Number of TCP packets sent
- `tcp_tx_bytes`: Estimated bytes sent via TCP
- `tcp_rx_pkts`: Number of TCP packets received
- `tcp_rx_bytes`: Estimated bytes received via TCP
- `udp_tx_pkts`: Number of UDP packets sent
- `udp_tx_bytes`: Estimated bytes sent via UDP
- `udp_rx_pkts`: Number of UDP packets received
- `udp_rx_bytes`: Estimated bytes received via UDP

## Implementation Details

### Probe Points

The eBPF program attaches kprobes to the following functions:

- `ax_net::tcp::TcpSocket::send` (entry + return)
- `ax_net::tcp::TcpSocket::recv` (entry + return)
- `ax_net::udp::UdpSocket::send` (entry + return)
- `ax_net::udp::UdpSocket::recv` (entry + return)

### Symbol Resolution

Symbols are resolved dynamically from `/proc/kallsyms` using Rust v0 name
mangling fragments:

- TCP: `["6ax_net3tcp", "9TcpSocket", "9SocketOps4send"]`
- UDP: `["6ax_net3udp", "9UdpSocket", "9SocketOps4send"]`

Multiple monomorphized instances are found and probed (e.g., for different
buffer types like `ReadBuf`, `WriteBuf`, `VmBytes`, `IoVectorBufIo`).

### Packet vs Byte Counters

**Packet counters are fully accurate** - they increment at function entry and
reflect the actual number of send/recv calls.

**Byte counters are estimates** - due to fundamental limitations of kretprobe
and the Rust ABI for `Result<usize, AxError>`, we cannot reliably extract
actual byte counts. Instead, we estimate bytes as `packets × 64` (assuming
average 64-byte packets).

See [Known Limitations](#known-limitations) for details.

## Known Limitations

### Byte Counter Accuracy

**Current Implementation**: Byte counters use heuristic estimation
(64 bytes per packet) rather than actual transmitted byte counts.

**Why**: The functions return `Result<usize, AxError>` (16 bytes), which uses
the sret (structure return) calling convention. At kretprobe time, the return
register contains a pointer to the caller's stack frame, which:

1. May be unwound when kretprobe fires
2. Cannot be safely accessed due to BPF verifier restrictions
3. Cannot have pointer validity proven for arbitrary stack addresses

All attempts to dereference the sret pointer via `bpf_probe_read_kernel` fail.

**Impact**: Byte counters provide useful trending information for relative
comparisons but do not reflect actual byte counts. Packet counters remain
fully accurate.

**Future Solutions**:

1. **fentry/fexit + BTF** (requires Linux 5.5+, BTF-enabled kernel)
   - Direct typed access to return values
   - No ABI guessing required
   
2. **Entry/exit correlation via HashMap**
   - Store buffer lengths at entry, match at exit
   - Complex but works with current kprobe infrastructure

3. **Kernel module kprobes**
   - Direct pt_regs access and caller stack inspection
   - Loses eBPF portability and safety guarantees

### Architecture Support

- ✅ **x86_64**: Fully tested and working
- ✅ **aarch64**: Fully tested and working
- ✅ **riscv64**: Fully tested and working
- ⚠️ **loongarch64**: QEMU virtio bug causes crashes (unrelated to our code)

### Concurrent Access

The eBPF map uses non-atomic increments for performance. In SMP systems with
high network load, there may be rare race conditions causing slight
undercounting. This is acceptable for monitoring use cases where approximate
trending is sufficient.

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

The test performs TCP and UDP network operations and verifies that:
- All packet counters are non-zero
- All byte counters are non-zero (estimated values)
- Statistics reflect the performed operations

## Troubleshooting

### "no symbols found in /proc/kallsyms"

The StarryOS kernel may not have the probed functions. Verify symbols exist:

```bash
grep "6ax_net3tcp" /proc/kallsyms | grep "9TcpSocket" | grep "9SocketOps4send"
```

### All counters are zero

1. Check that network operations are actually occurring
2. Verify eBPF programs loaded: `cat /proc/net/bpf_kprobe`
3. Check for eBPF verifier errors in dmesg

### eBPF logger warning

```
[WARN] failed to initialize eBPF logger: AYA_LOGS not found
```

This is expected - the aya logging map is optional and does not affect
functionality. To enable eBPF logging, ensure `AYA_LOGS` map is present in
the loaded program.

## License

Dual MIT/GPL (required for eBPF programs)
