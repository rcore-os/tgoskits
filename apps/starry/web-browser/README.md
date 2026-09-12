# web-browser — NetSurf software-rendered web browser on StarryOS

Runs the [NetSurf](https://www.netsurf-browser.org/) web browser on StarryOS to
render real HTML/CSS pages, under a Weston compositor (DRM backend + pixman
software renderer) presented through virtio-gpu and viewable over QEMU VNC. This
is the CPU-rasterization + remote-view path: no GPU acceleration, everything
rendered on the CPU.

It reuses the exact Alpine-rootfs + qemu-user-apk staging model proven by the
`qt-calc` app; only the installed packages (NetSurf + GTK3 stack instead of Qt6)
and the launched application differ.

## Architecture Support

Currently: **x86_64** (the graphics stack path first validated by qt-calc).

## Files

| File | Purpose |
|---|---|
| `prebuild.sh` | Resize rootfs, install Weston + NetSurf + GTK3 stack via qemu-user apk (shares the qt-calc apk cache) |
| `test.html` | Self-contained local page (HTML/CSS/tables) rendered without needing the network |
| `test_browser.sh` | Guest-side test: start Weston, launch NetSurf on the page, verify it renders without crashing |
| `qemu-x86_64.toml` | QEMU launch config: virtio-gpu + virtio-input, `-vnc`, 3G RAM, TCG |
| `build-x86_64-unknown-none.toml` | Kernel build features (display, input, virtio drivers) |

## Running

```bash
cargo xtask starry app qemu -t web-browser --arch x86_64
```

To watch the rendered page, connect a VNC viewer to the QEMU display (`-vnc :0`
= `127.0.0.1:5900`) while the guest is running, or capture a frame with
`scripts/visual-test/rfb_capture.py`.

## Test flow

1. `prebuild.sh` (host): stage an Alpine rootfs and `apk add` Weston + NetSurf +
   its GTK3/cairo/pango/gdk-pixbuf dependencies and fonts, then copy the tree
   into the overlay along with `test.html` and the guest test script.
2. `test_browser.sh` (guest): start Weston with the DRM backend and pixman
   renderer, serve `test.html` on `http://127.0.0.1:8080/`, launch NetSurf on it
   under Wayland, and fail unless the browser stays up for the whole render
   window. Set `BROWSER_URL` to load another page instead.
3. Success prints `WEB_BROWSER_TEST_PASSED`.

## Notes

- The Alpine `aliyun` mirror is used (empirically reliable here, same as
  qt-calc) rather than dl-cdn over the local proxy.
- NetSurf renders HTML/CSS and images but has no modern-JS engine; it is the
  lightest real graphical browser and the fastest way to get a page on screen.
  Heavier engines (badwolf/WebKit2GTK for JS, or Chromium) are follow-ups.
