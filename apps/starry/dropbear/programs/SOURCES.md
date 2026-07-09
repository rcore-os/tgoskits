# dropbear provenance

No binaries are committed. `programs/run-dropbear.sh` provisions the dropbear SSH suite
on-target with `apk add`, resolving the CURRENT package version from the Alpine branch that
matches the running rootfs (`/etc/alpine-release` -> `vMAJOR.MINOR`). apk pulls the full
dependency closure (musl / libutmps / libz), so no drifting URL is pinned and a clean
checkout reproduces the suite from Alpine's live index.

## Packages (Alpine `main` / `community`, branch-matched)

| package | binary staged | role |
|---------|---------------|------|
| `dropbear` | `/usr/sbin/dropbear`, `/usr/bin/dropbearkey` | SSH server + key generator |
| `dropbear-dbclient` | `/usr/bin/dbclient` | SSH client |
| `dropbear-convert` | `/usr/bin/dropbearconvert` | OpenSSH <-> dropbear key converter |
| `dropbear-scp` | `/usr/bin/scp` | scp over the dbclient transport |
| `dropbear-ssh` | `/usr/bin/ssh` | `ssh` -> `dbclient` symlink |

The rootfs base is Alpine 3.23 (`v3.23` main + community), which ships dropbear `2025.88-r1`
for all four arches (x86_64 / aarch64 / riscv64 / loongarch64).

This build ships `rsa`/`ecdsa`/`ed25519` key support only - DSS/DSA is compiled out (no
`ssh-dss` host-key algorithm, `dropbearkey -t dss` is rejected), matching dropbear upstream
dropping DSA. The `dbclient` forwarding surface is `-L`/`-R`/`-B`/`-g`; dropbear has no
dynamic-SOCKS `-D`. The carpet asserts both of these facts on-target rather than assuming
them.

## Mirror order

`apk add` tries the branch-matched repositories on
`https://mirrors.tuna.tsinghua.edu.cn/alpine` first, then
`https://dl-cdn.alpinelinux.org/alpine`. Override with `DROPBEAR_APK_MIRROR` /
`DROPBEAR_APK_BRANCH` if needed.

## Host pre-flight

The gate is environment-agnostic: where the suite is already present (a host Alpine chroot
or a warm rootfs) it skips apk and runs the identical carpet, so the exact same
`run-dropbear.sh` validates on the host before it runs under QEMU.
