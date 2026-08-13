# Orange Pi 5 Plus UVC Example

This case boots StarryOS on Orange Pi 5 Plus and runs a Rust `std` helper that
uses libuvc through manual FFI. The helper opens the first UVC camera, streams
MJPEG frames, and prints frame-rate and throughput statistics once per reporting
interval.

The helper is cross-compiled as a static AArch64 musl binary and uploaded with
the board session. `init.sh` downloads that session asset before starting the
test, so the case does not depend on `/usr/bin/uvc-fps` or any other persistent
board-rootfs preparation.

Run the board example:

```bash
cargo xtask starry app board -t orangepi-5-plus-uvc
```

The lifecycle regression keeps one libuvc device handle open while it executes
three `streaming -> alt 0 -> streaming` rounds. Each round receives frames,
stops the stream, and starts it again. Success requires the lifecycle marker,
non-zero aggregate frame/byte counters, and exactly one saved final frame:

```text
uvc-fps: lifecycle PASS rounds=3 active_alt=streaming->0->streaming pause_resume=ok async_iso_cancel_completion=ok
uvc-fps: done duration_sec=... frames=... avg_fps=... bytes=... saved=1 save_errors=0 avg_throughput_mib_s=...
-rw-r--r-- ... frame-000001.jpg
UVC_ENDPOINT_LIFECYCLE_OK
```

The source under `rust/` is built by the common Starry app asset pipeline.
`norm-uvc-sys` vendors libuvc and its native dependencies into the static
helper; the build and the board run therefore do not mutate the physical board
rootfs.
