# Orange Pi 5 Plus UVC Example

This case boots StarryOS on Orange Pi 5 Plus and runs a small Rust std program
that uses libuvc through manual FFI. The program opens the first UVC camera,
streams MJPEG frames, and prints frame-rate and throughput statistics once per
reporting interval. The Starry example command streams for 10 seconds, saves
only the final captured MJPEG frame to `/root/uvc-frames/frame-000001.jpg`, then
runs `sync` before returning.

The board rootfs must already contain:

- `/usr/bin/uvc-fps`, preferably built as a static
  `aarch64-unknown-linux-musl` binary
- access to the UVC camera through `/dev/bus/usb`

Run the board example:

```bash
cargo starry example board -t orangepi-5-plus-uvc
```

The runner succeeds when `uvc-fps` reports at least one non-zero statistics line,
for example:

```text
uvc-fps: frames=30 fps=30.00 bytes=1124960 saved=0 save_errors=0 throughput_mib_s=1.07 elapsed_sec=1.0
uvc-fps: final frame saved id=1 path=/root/uvc-frames/frame-000001.jpg bytes=12389 frame_id=300 sequence=300 size=320x240
uvc-fps: done duration_sec=10.0 frames=300 avg_fps=30.00 bytes=11249600 saved=1 save_errors=0 avg_throughput_mib_s=1.07
```

The `uvc-fps/` directory is a standalone Rust project with its own workspace.
Build and install it into the board rootfs separately before running the Starry
example; the example runner does not deploy rootfs assets.

For the Orange Pi board rootfs, build the helper in musl mode so Starry does
not need to load a glibc dynamic interpreter:

```bash
PKG_CONFIG_ALLOW_CROSS=1 \
PKG_CONFIG_ALL_STATIC=1 \
PKG_CONFIG_PATH=/path/to/aarch64-musl-sysroot/lib/pkgconfig \
PKG_CONFIG_LIBDIR=/path/to/aarch64-musl-sysroot/lib/pkgconfig \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc \
RUSTFLAGS="-C target-feature=+crt-static" \
cargo build --manifest-path uvc-fps/Cargo.toml \
  --release \
  --target aarch64-unknown-linux-musl
```
