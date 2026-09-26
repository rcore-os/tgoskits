# dnsmasq provenance

No binaries are committed. `prebuild.sh` provisions dnsmasq and the TFTP client the
reproducible way, resolving the CURRENT package versions from the Alpine branch that matches
the running rootfs (`/etc/alpine-release` -> `vMAJOR.MINOR`) out of the live APKINDEX. No
drifting URL is pinned and a clean checkout reproduces the binaries from Alpine's live index.
On a fresh rootfs the gate can instead `apk add dnsmasq tftp-hpa` on-target, which pulls the
same closure.

## Packages (Alpine `main`, branch-matched)

| package | binary staged | role |
|---------|---------------|------|
| `dnsmasq` | `/usr/sbin/dnsmasq` | DNS forwarder + authoritative records + DHCP/TFTP server |
| `tftp-hpa` | `/usr/bin/tftp` | TFTP client that drives the integrated TFTP server end to end |

Both `dnsmasq` and `tftp-hpa` NEED only `libc.musl`, already in the Alpine base rootfs, so no
shared library is staged. The DNS clients (`busybox nslookup`, which accepts `-type=` for
a/aaaa/cname/mx/txt/ptr/srv) and the DHCP client (`busybox udhcpc`) are base busybox applets,
so nothing else is staged. The binaries stay musl-dynamic on all four arches.

The rootfs base is Alpine 3.23 (`v3.23` main), which ships dnsmasq `2.91-r1` and tftp-hpa
`5.2-r7` for x86_64 / aarch64 / riscv64 / loongarch64.

## Mirror order

`prebuild.sh` resolves the index and apk from `https://dl-cdn.alpinelinux.org/alpine` first,
then `https://mirrors.tuna.tsinghua.edu.cn/alpine`. The on-target `apk add` fallback tries
the tuna mirror first, then the CDN. Override with `DNSMASQ_APK_MIRROR` / `DNSMASQ_APK_BRANCH`.

## Host pre-flight

The gate is environment-agnostic: where dnsmasq is already present (the staged overlay, a
host Alpine chroot, or a warm rootfs) it skips apk and runs the identical carpet, so the exact
same `run-dnsmasq.sh` validates on the host before it runs under QEMU.
