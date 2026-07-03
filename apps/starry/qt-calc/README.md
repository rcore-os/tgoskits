# Qalculate-QT — Qt6 Calculator Test for StarryOS

Tests Qt6/Wayland integration on StarryOS by running the
[Qalculate-QT](https://qalculate.github.io/) calculator under the
Weston compositor with the DRM backend (pixman software renderer).

## Architecture Support

Currently supports: **x86_64**

## Files

| File | Purpose |
|---|---|
| `prebuild.sh` | Resizes rootfs, installs Weston + Qt6 + qalculate-qt via qemu-user apk |
| `test_qcalc.sh` | Guest-side test script (copied to `/usr/bin/test-qcalc.sh`) |
| `qemu-x86_64.toml` | QEMU launch config: virtio-gpu, virtio-input, 2G RAM, TCG |
| `build-x86_64-unknown-none.toml` | Kernel build features (display, input, virtio drivers) |
| `README.md` | This file |

## Running the Test

```bash
# From the tgoskits workspace root
cargo xtask starry app qemu -t qt-calc --arch x86_64
```

For a clean rebuild (discard cached rootfs):

```bash
rm -rf /tmp/.tgos-images/rootfs-x86_64-alpine.img
cargo xtask starry app qemu -t qt-calc --arch x86_64
```

## Test Flow

### Host-side (prebuild.sh)

1. Extracts the base Alpine rootfs (v3.23) into a staging directory
2. Resizes the rootfs image from 1 GiB to 2 GiB
3. Installs packages via `qemu-user-static` apk:
   - Weston compositor (drm-backend, desktop-shell)
   - Qt6 base + Qt6 Wayland plugin
   - `qalculate-qt` (the calculator app)
   - Fonts and input libraries (libinput, libxkbcommon, pixman)
4. Copies installed files from staging into an overlay directory
5. Injects the overlay into the rootfs image

### Guest-side (test_qcalc.sh)

1. Verifies pre-installed packages are present
2. Checks that `/dev/dri/card0` exists and input devices are available
3. Starts Weston with DRM backend + pixman renderer
4. Waits for the Wayland socket (up to 120 s)
5. Launches `qalculate-qt` under Wayland (`QT_WAYLAND_DISABLE_EGL=1`)
6. Verifies the application exits without error
7. Checks the compositor log for warnings
8. Reports `QT_CALC_TEST_PASSED` on success

## Success Criteria

- Weston compositor starts successfully and creates Wayland socket
- Qt6 application (`qalculate-qt`) launches and runs without segfault
- No panics or critical errors
- Test prints `QT_CALC_TEST_PASSED`

## Known Limitations

1. **x86_64 only** — other architectures not yet supported
2. **Software rendering** — Weston uses pixman, Qt6 uses wl_shm (no GL/EGL)

## References

- [Qalculate-QT](https://qalculate.github.io/)
- [Weston Compositor](https://wayland.freedesktop.org/)
- [Alpine Linux Packages](https://pkgs.alpinelinux.org)
