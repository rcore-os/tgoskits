# starrywrt - the StarryWRT distribution on the StarryOS kernel

StarryWRT is an OpenWrt-userland distribution whose kernel is StarryOS (single-core) instead of
Linux. Apart from the kernel it is meant to be indistinguishable from OpenWrt: the same config
system (uci), the same package manager (opkg), the same SSH (dropbear) and DNS/DHCP (dnsmasq)
services, the same busybox base, and the same `/etc` identity and layout - all built from the
OpenWrt upstream sources, unmodified.

This app assembles that distribution into the rootfs, boots it on StarryOS across all four
architectures, and verifies that every shipped software stack runs correctly on the single-core
StarryOS kernel.

## What is assembled (prebuild.sh)

| stack | provenance | role |
|-------|------------|------|
| **uci** | cross-compiled from OpenWrt source (pinned, static musl) | configuration |
| **opkg** | cross-compiled from OpenWrt source (pinned, static musl) | `.ipk` packages |
| **dropbear** suite | Alpine apk (current version, branch-matched) | SSH server + client |
| **dnsmasq** | Alpine apk | DNS / DHCP |
| **busybox** | Alpine base rootfs | base userland |
| `/etc` layout | `files/` (banner, `openwrt_release`, `os-release`, `config/*`, `rc.common`, `init.d/*`) | OpenWrt identity + config + init framework |

No binaries are committed: uci/opkg are built from source, the service binaries come from apk.
The `/etc/config/*` files are the OpenWrt defaults (system/network/dhcp/firewall/dropbear).

## What is verified (the boot gate: run-starrywrt.sh)

The gate prints the StarryWRT banner, then runs three carpets to their pinned `OK` lines and
prints `TEST PASSED` only if all pass:

- **uci-carpet.sh** (70 assertions) - the full uci command + option surface.
- **opkg-carpet.sh** (42 assertions) - the full opkg `.ipk` lifecycle, offline.
- **starrywrt-carpet.sh** (52 assertions) - the distribution: OpenWrt identity files, the
  busybox base, the shipped `/etc/config` parsed by uci, the OpenWrt init framework
  (`/etc/rc.common` + `/etc/init.d/*`), the dropbear SSH stack (host-key generation + public
  key + fingerprint), and the dnsmasq stack (version + config syntax check).

## Run

```
cargo xtask starry app qemu -t starrywrt --arch x86_64
cargo xtask starry app qemu -t starrywrt --arch aarch64
cargo xtask starry app qemu -t starrywrt --arch riscv64
cargo xtask starry app qemu -t starrywrt --arch loongarch64
```

## Relationship to the other OpenWrt apps

`apps/starry/openwrt` carpets uci + opkg in isolation; `apps/starry/dropbear` carpets the SSH
suite. StarryWRT integrates them - plus dnsmasq, busybox and the OpenWrt `/etc` layout - into a
single bootable distribution and checks the whole userland together.
