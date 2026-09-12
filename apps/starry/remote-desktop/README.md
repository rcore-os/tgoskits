# remote-desktop

A headless remote desktop on StarryOS with no GPU device. Xvnc (TigerVNC) is an
X server whose framebuffer lives in RAM - no `/dev/dri`, no DRM/KMS, no GPU - and
which speaks VNC directly on its RFB port. A window manager (twm) and terminal
(xterm) render into that software framebuffer; QEMU forwards a host port to the
guest's Xvnc, so the whole desktop is reached remotely over the network while
every pixel is rasterized on the CPU.

Xvnc is used instead of `Xvfb + x11vnc` on purpose: x11vnc connects to the X
server as a client (`XOpenDisplay`), which fails on StarryOS even though other X
clients (xsetroot) connect fine over the same socket. Xvnc has no such client
step - it *is* the X server and the VNC server in one process - so the whole
pipeline is a single software process with no separate display connection.

## Run

```
cargo xtask starry app qemu -t remote-desktop --arch x86_64
```

The test starts `Xvnc :99` (software framebuffer + VNC on 5900), sets a banner
background, launches twm + xterm, brings the guest network up, and holds the
frame. `REMOTE_DESKTOP_TEST_PASSED` is printed once the desktop is up.

## View / drive the remote desktop from the host

QEMU forwards host `127.0.0.1:5901` to the guest's `5900`. While the test holds
the frame (after `REMOTE_DESKTOP_WINDOW_OPEN`), connect any VNC client:

```
vncviewer 127.0.0.1:5901
```

Input from the VNC client is injected by Xvnc into the X server, so the desktop
is interactive (clicking/typing into xterm) - this path does not use the guest's
virtio-input device at all.

## Contrast with the web-browser app

- `web-browser`: uses a virtio-gpu device + Weston DRM backend (card0 scanout),
  captured through QEMU's own `-vnc`. Depends on GPU device virtualization; input
  would depend on virtio-input.
- `remote-desktop`: no GPU device. Xvnc software framebuffer + QEMU hostfwd. Pure
  CPU rendering, viewed and driven over the network, independent of virtio-gpu
  and virtio-input.

Currently x86_64 only.
