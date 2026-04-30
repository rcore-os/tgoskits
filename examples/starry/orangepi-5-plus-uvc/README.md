# Orange Pi 5 Plus UVC Example

This case boots StarryOS on Orange Pi 5 Plus and runs a small Rust std program
that uses libuvc through manual FFI. The program opens the first UVC camera,
streams MJPEG frames, and prints frame-rate and throughput statistics once per
reporting interval. The Starry example command streams for 10 seconds, saves
only the final captured MJPEG frame to `/root/uvc-frames/frame-000001.jpg`, then
runs `sync` before returning.

The board rootfs must already contain:

- `/usr/bin/uvc-fps`
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

## Prepare the Board Rootfs

The simplest way to prepare the Orange Pi rootfs is to reserve the board, let it
boot Linux, build the helper natively, and install it into `/usr/bin`.

Keep the serial session open while preparing the board. This reserves the board
lease so another run cannot take it in the middle of the copy/build flow:

```bash
cargo board connect --board-type OrangePi-5-Plus
```

Wait for the Orange Pi Linux login shell. The login banner prints the IP address,
for example:

```text
IP: 10.3.10.219
```

If the banner scrolls away, query it from the serial shell:

```bash
ip -br addr
```

Use that address from another host terminal. The examples below use
`BOARD_IP=10.3.10.219`; replace it with the address printed by your board.

If passwordless SSH is not already configured, install a temporary host key into
the Linux user from the serial shell:

```bash
mkdir -p ~/.ssh
chmod 700 ~/.ssh
cat >> ~/.ssh/authorized_keys
```

Paste your host public key, press Enter, then Ctrl-D. Finish with:

```bash
chmod 600 ~/.ssh/authorized_keys
```

Copy the helper source from the host to the board:

```bash
export BOARD_IP=10.3.10.219

ssh orangepi@${BOARD_IP} 'rm -rf ~/tgoskits-uvc-fps && mkdir -p ~/tgoskits-uvc-fps'
rsync -az --delete \
  examples/starry/orangepi-5-plus-uvc/uvc-fps/ \
  orangepi@${BOARD_IP}:~/tgoskits-uvc-fps/
```

Build it on the board Linux system:

```bash
ssh orangepi@${BOARD_IP} '
  cd ~/tgoskits-uvc-fps &&
  rm -f Cargo.lock &&
  cargo build --release
'
```

`rm -f Cargo.lock` is intentional for older board images whose Cargo cannot read
lockfile version 4. The helper only needs the `pkg-config` build dependency, so
the board can regenerate a compatible lockfile locally.

Install the binary into the rootfs used by StarryOS:

```bash
ssh orangepi@${BOARD_IP} '
  sudo install -m 0755 \
    ~/tgoskits-uvc-fps/target/release/uvc-fps \
    /usr/bin/uvc-fps &&
  /usr/bin/uvc-fps --help | head
'
```

If the board requires a sudo password in non-interactive SSH, pass it through
stdin:

```bash
ssh orangepi@${BOARD_IP} "
  printf '%s\n' orangepi | sudo -S install -m 0755 \
    ~/tgoskits-uvc-fps/target/release/uvc-fps \
    /usr/bin/uvc-fps
"
```

Run a short Linux-side smoke test before booting StarryOS. Root is usually needed
because `/dev/bus/usb` is not writable by the default `orangepi` user:

```bash
ssh orangepi@${BOARD_IP} "
  rm -rf /tmp/uvc-frames &&
  mkdir -p /tmp/uvc-frames &&
  printf '%s\n' orangepi | sudo -S \
    /usr/bin/uvc-fps \
      --device 0 \
      --format mjpeg \
      --width 320 \
      --height 240 \
      --fps 30 \
      --interval-sec 1 \
      --duration-sec 3 \
      --save-dir /tmp/uvc-frames \
      --save-last \
      --max-saved 1 &&
  ls -lh /tmp/uvc-frames
"
```

Expected Linux-side output includes a final success line:

```text
uvc-fps: done duration_sec=3.0 frames=80 avg_fps=26.67 bytes=1078708 saved=1 save_errors=0 avg_throughput_mib_s=0.34
```

After the helper is installed, close the `cargo board connect` serial session so
the board lease is released. Then run the Starry example from the repository
root:

```bash
cargo starry example board -t orangepi-5-plus-uvc
```

## Optional Cross Build

The native board build above produces an AArch64 glibc binary. That is the most
direct path for the shared Orange Pi rootfs. If you need a static helper for a
different rootfs, build in musl mode on the host:

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
