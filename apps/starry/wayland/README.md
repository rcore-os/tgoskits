# Starry Wayland/Weston App

This app case runs Weston, the reference Wayland compositor, on StarryOS with
QEMU virtio GPU and input devices. The automated test proves the compositor can
start on the DRM backend and accept a Wayland client connection. The manual
flow below starts the same stack with a VNC display so `gtk4-demo` can be used
interactively.

## Host Prerequisites

- QEMU with the target system emulators you want to run.
- Rust nightly and the normal repository build prerequisites.
- `debugfs` from e2fsprogs. On macOS with Homebrew:

```bash
brew install e2fsprogs
export PATH="/opt/homebrew/opt/e2fsprogs/sbin:$PATH"
```

Run all commands from the repository root.

## Automated Test

Run the Starry app test through `xtask`:

```bash
cargo xtask starry app qemu -t wayland --arch riscv64
cargo xtask starry app qemu -t wayland --arch x86_64
cargo xtask starry app qemu -t wayland --arch aarch64
cargo xtask starry app qemu -t wayland --arch loongarch64
```

The successful output contains both markers:

```text
WAYLAND_TEST_RESULT PASSED
WAYLAND_TEST_PASSED
```

The app prebuild step downloads the heavy `llvm21-libs` APK dependency closure
on the host and injects those APKs into the rootfs overlay. The guest script is
[`wayland-test.sh`](wayland-test.sh). It installs any prefetched APKs first,
then installs `weston`, `weston-backend-drm`, and `weston-shell-desktop` from
Alpine apk with bounded HTTP mirror fallback, checks that `/dev/dri/card0` is
present, checks for `/dev/input/event*`, starts Weston with the DRM/pixman
backend, waits for `/tmp/wayland-*`, connects a client when `weston-info` is
available, scans the Weston log for obvious startup errors, and shuts the
compositor down cleanly.

The automated test exercises these kernel paths:

| Subsystem | Device / Syscall | Notes |
|-----------|------------------|-------|
| DRM/KMS | `/dev/dri/card0` | Dumb buffers, modeset, page flip |
| Input | `/dev/input/event*` | evdev protocol, libinput probe |
| memfd | `memfd_create` | Wayland SHM buffer backing storage |
| eventfd | `eventfd` | Compositor event loop signalling |
| Unix sockets | `bind` / `sendmsg` / `SCM_RIGHTS` | Wayland socket and fd passing |

## Manual Reproduction with VNC

The manual flow intentionally avoids the app test's `shell_init_cmd`; it boots
the same kernel and Alpine rootfs directly so you can type commands at the
StarryOS shell and interact with GTK through VNC. The guest-side Weston and GTK
commands are the same for the direct riscv64 and x86_64 flows. Only the
host-side QEMU launch command differs. For aarch64, use the helper documented
in the aarch64 note below.

### Step 1: Build the Kernel and Provisioned Rootfs

Run the automated test once for the architecture you want to reproduce. This
creates the kernel image and rootfs image used by the direct QEMU commands. It
also installs the prefetched Weston dependency closure into that rootfs, so the
manual session does not need guest networking.

```bash
export PATH="/opt/homebrew/opt/e2fsprogs/sbin:$PATH"
ARCH=riscv64   # or: x86_64
cargo xtask starry app qemu -t wayland --arch "$ARCH"
```

### Step 2: Copy the Rootfs for the Manual Session

Use a copy so package installation and manual experiments do not dirty the
rootfs used by the app runner.

```bash
mkdir -p tmp/wayland-manual
cp "tmp/axbuild/rootfs/rootfs-${ARCH}-alpine.img" "tmp/wayland-manual/${ARCH}.img"
```

### Step 3: Start QEMU with a VNC Display

Choose the launch command that matches `ARCH`. The differences are QEMU binary,
machine type, and kernel image path. Choose any free VNC display number; the
TCP port is `5900 + VNC_DISPLAY`.

```bash
VNC_DISPLAY=30  # example; use any free QEMU VNC display number
VNC_PORT=$((5900 + VNC_DISPLAY))
```

For RISC-V:

```bash
qemu-system-riscv64 \
  -machine virt \
  -kernel target/riscv64gc-unknown-none-elf/release/starryos.bin \
  -m 1G \
  -cpu rv64 \
  -serial stdio \
  -monitor none \
  -vnc "127.0.0.1:${VNC_DISPLAY}" \
  -device virtio-gpu-pci \
  -device virtio-keyboard-pci \
  -device virtio-mouse-pci \
  -device virtio-blk-pci,drive=disk0 \
  -drive id=disk0,if=none,format=raw,file=tmp/wayland-manual/riscv64.img
```

For x86_64, use `xtask` to launch QEMU because the current dynamic x86_64
platform boots through generated OVMF/ESP artifacts rather than direct
`qemu-system-x86_64 -kernel starryos`. Create a manual VNC QEMU config, replacing
`<display>` with `VNC_DISPLAY`:

```toml
args = [
  "-m", "1G",
  "-serial", "stdio",
  "-monitor", "none",
  "-vnc", "127.0.0.1:<display>",
  "-machine", "q35",
  "-device", "virtio-gpu-pci",
  "-device", "virtio-keyboard-pci",
  "-device", "virtio-mouse-pci",
  "-device", "virtio-blk-pci,drive=disk0",
  "-drive", "id=disk0,if=none,format=raw,file=${workspace}/tmp/wayland-manual/x86_64.img",
]
uefi = true
to_bin = true
timeout = 900
fail_regex = ["(?i)\\bpanic(?:ked)?\\b"]
```

Then launch it:

```bash
cargo xtask starry app qemu \
  -t wayland \
  --arch x86_64 \
  --qemu-config tmp/wayland-manual/qemu-x86_64-vnc.toml
```

Wait for the serial console to print the `root@starry:` prompt.

### Step 4: Open the VNC Viewer

Open the display from the host:

```bash
open "vnc://127.0.0.1::${VNC_PORT}"
```

Some VNC clients prefer `127.0.0.1:${VNC_PORT}` when entering the address
manually. The double-colon form is the explicit TCP-port form used by many
command-line VNC tools.

### Step 5: Verify User-Space Packages

If Step 1 completed successfully, the copied rootfs already contains Weston and
its runtime dependencies. At the `root@starry:` prompt:

```sh
command -v weston
ls /usr/lib/libweston-*/drm-backend.so
ls /usr/lib/weston/desktop-shell.so
```

Install `gtk4-demo` only if it is not already present in the manual image:

```sh
if ! command -v gtk4-demo >/dev/null 2>&1; then
apk_branch="$(sed -n 's#.*/\(v[0-9][0-9.]*\)/main#\1#p' /etc/apk/repositories | head -1)"
[ -n "$apk_branch" ] || apk_branch=v3.22

for mirror in \
  http://mirrors.huaweicloud.com/alpine \
  http://dl-cdn.alpinelinux.org/alpine \
  http://mirrors.aliyun.com/alpine \
  http://mirrors.tuna.tsinghua.edu.cn/alpine \
  http://mirrors.cernet.edu.cn/alpine
do
  printf '%s/%s/main\n%s/%s/community\n' \
    "$mirror" "$apk_branch" "$mirror" "$apk_branch" >/etc/apk/repositories
  apk add --no-cache weston weston-backend-drm weston-shell-desktop gtk4.0-demo font-dejavu && break
done
fi
```

The install loop is only for a rootfs that does not already contain the manual
GTK demo packages and a kernel/QEMU launch that has working guest networking.
The standard Wayland app build is intentionally offline and does not attach a
virtio-net device. The loop installs Weston, the DRM backend plugin, the
desktop shell plugin, GTK4, Mesa, libdrm, libinput, a usable GTK font, and their
runtime dependencies.
The HTTP mirrors intentionally avoid guest TLS certificate trust failures when
the emulated RTC starts at an invalid date. If one mirror stalls or returns a
truncated package, rerun the loop; the next mirror will be tried.

### Step 6: Start Weston

Still inside StarryOS:

```sh
export XDG_RUNTIME_DIR=/tmp
chmod 0700 /tmp
export LIBSEAT_BACKEND=noop
rm -f /tmp/wayland-*

weston \
  --backend=drm-backend.so \
  --renderer=pixman \
  --no-config \
  --idle-time=0 \
  --log=/tmp/weston.log &
```

Expected evidence in `/tmp/weston.log` includes a `Virtual-1` DRM head and an
enabled output. Confirm that the Wayland socket exists:

```sh
ls -l /tmp/wayland-*
```

### Step 7: Start GTK4 Demo

```sh
export WAYLAND_DISPLAY="$(basename "$(ls /tmp/wayland-* | head -1)")"
export GDK_BACKEND=wayland
export GSK_RENDERER=cairo
gtk4-demo &
ps | grep gtk4-demo
```

The GTK4 demo window should appear in the VNC viewer. Use the VNC mouse and
keyboard to click widgets, open demo rows, scroll lists, and close or reopen
demo windows. For additional compositor evidence:

```sh
tail -100 /tmp/weston.log
```

### Step 8: Optional SHM Client Check

If `weston-simple-shm` is present in the image, it can be used as a small SHM
rendering client:

```sh
weston-simple-shm &
```

### Step 9: Shut Down

```sh
pkill gtk4-demo || true
pkill weston || true
poweroff
```

## aarch64 Note

The automated Wayland app test does not require guest networking: the heavy
APK dependency closure is prefetched on the host and injected into the rootfs
overlay before boot. With that offline setup the aarch64 app test follows the
same `cargo xtask starry app qemu -t wayland --arch aarch64` flow as the other
architectures.

The Cocoa/VNC helper is kept at [`run-hvf.sh`](run-hvf.sh). It uses the same
host-prefetched APK cache approach for Weston and `gtk4-demo`, expands the
manual rootfs, provisions it offline on first run, and then reuses the
provisioned image on later runs:

```bash
./apps/starry/wayland/run-hvf.sh --no-build --provision-only
STARRY_VNC=9 ./apps/starry/wayland/run-hvf.sh --no-build --vnc-only
```

Use `--reprovision` to discard and recreate
`tmp/axbuild/rootfs/rootfs-aarch64-wayland.img`. Set
`STARRY_WAYLAND_ROOTFS_MB` if the default 4096 MiB manual image is not suitable.
The helper requires host `debugfs`, `e2fsck`, `resize2fs`, `python3`, and
`qemu-system-aarch64`; on macOS with Homebrew, the script adds the usual
Homebrew paths automatically.
The normal helper path does not attach a guest virtio-net device because the
Wayland build config is intentionally offline and does not enable network
drivers.
Use `--vnc-only` when you want to drive the serial console from the terminal and
view the GUI through VNC without Cocoa taking terminal focus.

## Kernel-Side Dependencies

This app requires:

- DRM `/dev/dri/card0` support with dumb buffer allocation.
- virtio GPU, keyboard, mouse, and block devices in the QEMU config.
- evdev `/dev/input/event*` support for libinput.
- `memfd_create` and file-descriptor passing over Unix sockets for Wayland SHM.
- `eventfd` for the compositor event loop.
- udev seed data under `/run/udev/data/` for libinput device discovery.
- `starry-kernel/input` and `ax-feat/display` in the app build config.

The optional manual package-install flow additionally needs a kernel/QEMU launch
with working guest networking if the packages are not already present in the
copied rootfs.
