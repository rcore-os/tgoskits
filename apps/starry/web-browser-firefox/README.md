# web-browser-firefox — Firefox ESR on StarryOS

Runs [Firefox ESR](https://www.mozilla.org/firefox/enterprise/) on StarryOS and
loads `https://www.4399.com/` over a real TLS connection, under a Weston
compositor (DRM backend + pixman software renderer) presented through virtio-gpu
and viewable over QEMU VNC. Everything is rasterized on the CPU: Gecko runs with
software WebRender, and no GPU acceleration is involved.

It shares the Alpine-rootfs + qemu-user-apk staging model with the sibling
`web-browser` app; the installed packages (Firefox ESR + Mesa software GL + NSS)
and the launched browser are what differ.

## Architecture Support

Currently: **x86_64**.

## Files

| File | Purpose |
|---|---|
| `prebuild.sh` | Resize rootfs, install Weston + Firefox ESR + Mesa/GTK/NSS stack via qemu-user apk |
| `test_browser.sh` | Guest-side test: start Weston, launch Firefox, and require evidence that a page was actually fetched and laid out |
| `qemu-x86_64.toml` | QEMU launch config: virtio-gpu + virtio-input, `-vnc`, 6G RAM, KVM when the host offers it and TCG otherwise |
| `build-x86_64-unknown-none.toml` | Kernel build features (display, input, virtio drivers) |

## Running

```bash
cargo xtask starry app qemu -t web-browser-firefox --arch x86_64
```

To watch the page being rendered, connect a VNC viewer to the QEMU display
(`-vnc :0` = `127.0.0.1:5900`) while the guest is running, or capture a frame
with `scripts/visual-test/rfb_capture.py`.

## Reaching the network

The guest reaches the public web through QEMU user networking. Where that path
is filtered, the test also probes a host relay on `10.0.2.2:8899` and routes
Firefox through it when the relay answers; the run reports which path it took.
Set `BROWSER_URL` to point the same build at a different page.

## What the test proves

The run does not pass merely by staying alive. It requires the profile's cache
to show entries the browser wrote for the page it was asked to open, so a blank
window, a browser that died on its first frame, and a load that never started
are told apart rather than all reported as success.
