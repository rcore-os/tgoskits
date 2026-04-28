# Orange Pi 5 Plus UVC Example

This case boots StarryOS on Orange Pi 5 Plus and starts a preinstalled UVC frame
rate reporter from the Starry shell.

The board rootfs must already contain:

- `/usr/bin/uvc-fps`
- `libuvc` and `libusb` runtime libraries
- access to the UVC device through `/dev/bus/usb`

Run the board example:

```bash
cargo starry example board -t orangepi-5-plus-uvc
```

Build the sample user program separately when preparing the rootfs:

```bash
cargo build \
  --manifest-path examples/starry/orangepi-5-plus-uvc/uvc-fps/Cargo.toml \
  --release \
  --target aarch64-unknown-linux-musl
```

Install the resulting `uvc-fps` binary into `/usr/bin/uvc-fps` in the Starry
rootfs together with the `libuvc` and `libusb` libraries it needs.
