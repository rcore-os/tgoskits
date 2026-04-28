# Orange Pi 5 Plus USB Enumeration Example

This case boots StarryOS on Orange Pi 5 Plus and runs `lsusb` from the Starry
shell. It is intended to compare Starry USB enumeration with the Linux baseline,
including root hub and attached USB camera visibility.

The board rootfs must already contain:

- `lsusb`
- access to USB devices through `/dev/bus/usb`

Run the board example:

```bash
cargo starry example board -t orangepi-5-plus-uvc
```

The runner succeeds when the `lsusb` output contains a Linux Foundation root hub
line, for example:

```text
Bus 001 Device 001: ID 1d6b:0002 Linux Foundation 2.0 root hub
```

The `uvc-fps/` Rust project remains in this case as a supplemental libuvc smoke
program, but the automated example currently uses `lsusb` only.
