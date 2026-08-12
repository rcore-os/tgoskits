# Zephyr and Linux VirtIO-Net Peers

This QEMU AArch64 scenario validates the internal AxVisor VirtIO-net switch
using a Linux guest and a Zephyr guest. It is intentionally an isolated guest
network: it has no host NIC, TAP, bridge, or NAT uplink.

## Build guest images

Use the sibling `tgosimages` checkout. The `zephyr-net` target builds Zephyr's
TCP echo server with its VirtIO-MMIO network node enabled. It also stages both
guest images under `/guest` in the selected rootfs.

```bash
cd ../tgosimages
./build.sh platform qemu-aarch64 linux --rootfs alpine
./build.sh platform qemu-aarch64 zephyr-net --rootfs alpine
```

The Linux build enables `VIRTIO`, `VIRTIO_MMIO`, and `VIRTIO_NET` as built-in
drivers. The Zephyr guest is configured as `192.0.2.2/24` and runs the echo
server on TCP port `4242`.

## Run and verify

```bash
cd ../tgoskits
TGOSIMAGES_DIR=../tgosimages \
  os/axvisor/scripts/run-qemu-aarch64-zephyr-linux-virtio-net.sh
```

At the Linux guest shell, run the staged payload check:

```sh
sh /guest/linux/virtio-net-verify.sh
```

The script assigns `192.0.2.1/24` to Linux, checks the echoed text payload,
then checks a 64 KiB response. It prints `LINUX_ZEPHYR_VIRTIO_NET_PASS` only
when both checks succeed. This covers ARP, IPv4, TCP, guest TX/RX queues, and
the receive interrupt path in both directions.

## Configuration

The board configuration starts a Linux VM on physical CPU 1 and Zephyr on
physical CPU 2. Both VMs declare `virtnet0`, with distinct locally administered
MAC addresses. AxVisor publishes the matching `virtio,mmio` node at
`0x0a00_0000` and GIC SPI 16 in each runtime device tree.
