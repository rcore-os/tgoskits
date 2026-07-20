# openwrt - OpenWrt userland carpet (uci + opkg) on StarryOS

`uci` (the Unified Configuration Interface) and `opkg` (the `.ipk` package manager) are the two
signature userland tools of an OpenWrt system - the config layer and the package layer that sit
on top of the kernel. This app runs both, carpet-level, on StarryOS across all four
architectures, proving StarryOS can host the OpenWrt userland tooling.

It joins the OpenWrt components already carried in `apps/starry`: `dropbear` (SSH) and `dnsmasq`
(DNS/DHCP). Together they form the core of an OpenWrt-style userland running on the StarryOS
kernel.

## Delivery model

Neither `uci` nor `opkg` is packaged by Alpine, so - unlike the apk-based apps - `prebuild.sh`
**cross-compiles them from the OpenWrt upstream C source in-prebuild**, the same way the language
apps cross-build their native bits:

- the per-arch musl cross-toolchain is supplied out-of-band (StarryOS `.starry-env.sh` PATH),
- the sources are pinned to immutable commits (reproducible - no drifting `HEAD`, no committed
  binaries), and
- only the resulting static-musl binaries are staged into the overlay.

Dependency chain (all small cmake C, static musl):

```
json-c  ->  libubox  ->  uci    (cli target; BUILD_STATIC)
            libubox  ->  opkg   (opkg-cl; STATIC_UBOX - opkg ships its own libbb .ipk
                                 extractor so it needs no libarchive, and downloads via a
                                 wget shell-out so it needs no libcurl: libubox + pthread
                                 is the entire dependency set)
```

Pinned sources:

| project | upstream | commit |
|---------|----------|--------|
| json-c  | https://github.com/json-c/json-c        | `324e5ca5` |
| libubox | https://git.openwrt.org/project/libubox.git  | `17f527fb` |
| uci     | https://git.openwrt.org/project/uci.git      | `74f6277a` |
| opkg    | https://git.openwrt.org/project/opkg-lede.git | `80503d94` |

## What the carpet exercises

Both carpets are hermetic and offline (no guest network, no committed packages). The gate
(`programs/run-openwrt.sh`) runs both and prints `TEST PASSED` only when each reports its pinned
`OK <n>` line - each carpet prints that line only when every assertion passed **and** the count
equals its pinned total, so a skipped assertion fails the gate.

- **uci** (`programs/uci-carpet.sh`, 70 assertions): the full command surface
  (`get`/`show`/`set`/`commit`/`add`/`add_list`/`del_list`/`delete`/`rename`/`reorder`/`revert`/
  `changes`/`export`/`import`/`batch`) and the option surface (`-c`/`-d`/`-q`/`-s`/`-S`/`-X`/
  `-n`/`-N`/`-f`) against a synthetic `/etc/config` tree.
- **opkg** (`programs/opkg-carpet.sh`, 42 assertions): the full package-manager surface
  (`update`/`list`/`install` with dependency resolution/`remove`/`upgrade`/`files`/`status`/
  `info`/`depends`/`whatdepends`/`flag`/`compare-versions` across every operator/
  `print-architecture` and the `--force-*` / `--offline-root` options) against a synthetic
  `.ipk` feed the carpet builds at runtime.

## Run

```
cargo xtask starry app qemu -t openwrt --arch x86_64
cargo xtask starry app qemu -t openwrt --arch aarch64
cargo xtask starry app qemu -t openwrt --arch riscv64
cargo xtask starry app qemu -t openwrt --arch loongarch64
```
