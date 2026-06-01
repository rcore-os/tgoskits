# Bundling Xwayland + xeyes for the visual-test rootfs

The scenario needs Alpine-edge Xwayland packages that the base weston
rootfs doesn't ship. CI materializes them from
`rootfs_extras.packages` before running the visual scenario:

```sh
python3 scripts/visual-test/prepare_rootfs_extras.py \
    --arch riscv64 \
    --scenario xwayland_xeyes
```

`run_all.sh` fails the scenario if `rootfs_extras.packages` exists but
`rootfs_extras/` was not generated, so a green CI run proves the
declared Xwayland dependencies were injected into the scratch rootfs.

Running the scenario without the bundle present in the rootfs produces
"0 non-black pixels" because Xwayland fails to load. The manual
extraction below is kept as a fallback/debugging recipe.

## Build for riscv64

```sh
mkdir -p /tmp/xwayland_riscv64
docker run --rm --platform linux/riscv64 \
    -v /tmp/xwayland_riscv64:/out \
    alpine:edge sh -c '
set -e
apk add --no-cache xwayland weston-xwayland xeyes xterm xauth \
    libxfont2 libtirpc-nokrb libxcvt util-linux libuuid \
    libepoxy mesa-gbm libmd libxshmfence \
    xkeyboard-config 2>&1 | tail -3
cd /
tar czf /out/bundle.tar.gz \
    usr/bin/Xwayland usr/bin/xeyes usr/bin/xterm usr/bin/xauth usr/bin/xkbcomp \
    usr/lib/libweston-14/xwayland.so \
    $(find usr/lib -maxdepth 1 -name "lib*.so.*" -o -name "lib*.so" | sort -u) \
    usr/share/X11 etc/netconfig \
    2>/dev/null
ls -lh /out/bundle.tar.gz
'
```

Bundle lands at `/tmp/xwayland_riscv64/bundle.tar.gz` (~86 MB). Inject
into the weston-enabled rootfs via e2tools (CI) or docker loop mount
(dev), same pattern the visual-test harness uses.

## Packages that matter, and what surfaces each one

| Package | Without it |
|---|---|
| `xwayland` | `Xwayland` binary doesn't exist in rootfs |
| `weston-xwayland` | weston's `libweston-14/xwayland.so` plugin missing — weston logs "xwayland-plugin module not loaded" and refuses `--xwayland` |
| `xeyes` | no X client to render |
| `libxfont2` | Xwayland reports `xfont2_*: symbol not found` at startup (library file is `libXfont2.so` — note capital X, easy to miss in glob patterns) |
| `libtirpc-nokrb` | `xdr_opaque_auth`, `_authenticate`, `xdrmem_create` symbols missing. **Must be the nokrb variant, not plain libtirpc** — Alpine's Xwayland binary links against `libtirpc-nokrb.so.3` specifically. |
| `libxcvt` | `libxcvt_gen_mode_info` not found |
| `libepoxy`, `mesa-gbm` | GL surface composition refs (libepoxy is the GL function loader, libgbm is buffer management) |
| `libmd` | newer weston/Xwayland need libmd's SHA1/MD5 helpers |
| `libxshmfence` | X DRI3 shared-memory fence primitive |
| `libuuid` + `util-linux` | libSM.so (session management) refs `uuid_generate` |

## aarch64 / x86_64

Same recipe with `--platform linux/arm64` or `--platform linux/amd64`.
Haven't wired those into the scenario's `arches` file yet — when
adding, bundle each separately and drop into the matching rootfs.
