# Sources

`prebuild.sh` cross-compiles the binaries from these pinned OpenWrt upstream commits (no
committed binaries; the musl cross-toolchain is provided out-of-band via `.starry-env.sh`):

| project | upstream | commit | role |
|---------|----------|--------|------|
| json-c  | https://github.com/json-c/json-c              | `324e5ca5937c459812973149f2c31ae25b6439bb` | libubox blobmsg_json dependency |
| libubox | https://git.openwrt.org/project/libubox.git   | `17f527fb6c30bf9073104f03337c2b7c03158bdb` | shared OpenWrt C base lib |
| uci     | https://git.openwrt.org/project/uci.git       | `74f6277aabffc943d026f406df57c22595134c42` | Unified Configuration Interface |
| opkg    | https://git.openwrt.org/project/opkg-lede.git | `80503d94e356476250adaf1f669ee955ec26de76` | `.ipk` package manager |

`uci-carpet.sh` and `opkg-carpet.sh` are original, doc-grounded against each tool's own usage
tree. The opkg carpet builds its synthetic `.ipk` feed at runtime with `tar`/`gzip` only (the
OpenWrt `.ipk` tar.gz container format), so it is hermetic and needs no network.
