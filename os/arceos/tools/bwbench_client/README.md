# Benchmark BandWidth Client

Benchmark BandWidth Client is a performance testing tool for measuring the network card's ability to send Ethernet packets. It can test both the transmission throughput and the reception throughput.

## Usage
In client:
```shell
cargo build --release
sudo ./target/release/bwbench_client [sender|receiver] [interface]
```

By reading the source code, you can control the behavior of the benchmark by modifying constants such as `MAX_BYTES`.

The matching ArceOS-side benchmark entry is no longer part of the core `ax-net`
public API. If a guest-side raw-frame benchmark is needed, keep it as a
separate app or test-suit case instead of wiring it back into the network stack.


## Example: benchmark bandwidth of QEMU tap netdev

In client:

```shell
cargo build --release
sudo ./scripts/net/qemu-tap-ifup.sh enp8s0
sudo ./target/release/bwbench_client [sender|receiver] tap0
```

On the guest side, run a dedicated raw-frame benchmark app or test case that
matches the selected sender/receiver mode.
