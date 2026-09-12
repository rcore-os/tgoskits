# dnsmasq - StarryOS DNS/DHCP/TFTP carpet

A carpet-level test of the [dnsmasq](https://thekelleys.org.uk/dnsmasq/doc.html) DNS
forwarder and DHCP/TFTP server on StarryOS. An authoritative instance serves every DNS
record class, a forwarder plus upstream pair proves upstream forwarding, caching (including
the live statistics dnsmasq dumps on `SIGUSR1`) and never-forward local zones, the DHCP
server config surface is validated through the real config parser, the integrated TFTP
server is driven end to end by a real client that fetches single- and multi-block files and
byte-checks them, and an integration instance combines hosts, local records and forwarding
the way a real edge resolver is deployed - all single-node over IPv4 loopback. The gate
provisions dnsmasq and a TFTP client on-target with `apk add` (branch-matched to the rootfs,
current version - no pinned URL) and queries DNS with base `busybox nslookup`.

## Run

```
cargo xtask starry app qemu -t dnsmasq --arch x86_64
cargo xtask starry app qemu -t dnsmasq --arch aarch64
cargo xtask starry app qemu -t dnsmasq --arch riscv64
cargo xtask starry app qemu -t dnsmasq --arch loongarch64
```

The gate (`programs/run-dnsmasq.sh`) is the entire `shell_init_cmd`. It prints `TEST PASSED`
only when every assertion passes AND the count equals the pinned `EXPECTED` total, so a
skipped or dropped assertion fails the gate. The last line is `DNSMASQ_OK=46/46`.

## Coverage (46 assertions)

| section | assertions | what is exercised |
|---------|-----------|-------------------|
| A. binary self-certification | A1-A6 | `--version` identity + compile options (DHCP/DHCPv6/TFTP/IPv6/auth); `--help` DNS+DHCP+TFTP option surface; `--help dhcp` option registry; `--test` validates a good config AND rejects a broken one - the real parser |
| B. DNS records (authoritative) | B1-B15 | hosts-file A; `--expand-hosts` domain suffix; `--address=/domain/` wildcard (two depths); `--txt-record`; `--cname`; `--mx-host`; `--srv-host`; `--ptr-record`; `--host-record` A + AAAA + auto reverse PTR; unknown-name rejection; `--pid-file` live pid; `--cache-size` honored - each queried for real over 127.0.0.1:53 |
| C. forwarding / caching / local | C1-C6 | forwarder + upstream both up; `--server=/domain/ip#port` zone forwarding (also proves custom `--port`); answer caching; `--server` default upstream; `--local=/domain/` never-forward zone; `SIGUSR1` live cache size + forwarded/answered-locally query statistics |
| D. DHCP config surface | D1-D10 | `--dhcp-range` (netmask + lease); `--dhcp-host` static MAC binding; `--dhcp-option` router/dns-server; `--dhcp-boot`/`--dhcp-authoritative`/`--read-ethers`/`--dhcp-lease-max`; `--dhcp-hostsfile`/`--dhcp-optsfile` from files; tag-scoped `--dhcp-range set:` + `--dhcp-option-force` + `--dhcp-vendorclass`; `--dhcp-mac`/`--dhcp-host ignore`/`--dhcp-ignore`; reservation-only range + `--dhcp-sequential-ip` + infinite lease; malformed range and malformed option both rejected - all through dnsmasq's real config parser |
| E. conf-file / conf-dir | E1-E2 | `--conf-file` record resolves; `--conf-dir` record resolves |
| F. integrated TFTP (real transfer) | F1-F4 | `--enable-tftp` server up on 127.0.0.1:69; a single-block file fetched by a real tftp client and byte-verified; a > 512-byte multi-block file fetched and byte-verified (block/ACK loop); a missing file rejected |
| G. integration | G1-G3 | one edge instance up with upstream; a `/etc/hosts` name and a forwarded upstream name both resolve through it; a local TXT and a local host-record A both served by the same instance |

Every DNS record and forwarded answer is queried against the real dnsmasq process over
loopback; every TFTP payload is transferred by a real client and byte-compared; the negative
paths (unknown name, malformed DHCP range, malformed DHCP option, missing TFTP file) are
asserted to be rejected.

## Scope

DNS resolution, forwarding, caching (with live `SIGUSR1` statistics), the record classes, the
config surface, and the integrated TFTP server (real single- and multi-block transfers) are
driven end to end over loopback. DHCP is covered through its real config parser (server-side
spec acceptance across ranges/static-hosts/options/hostsfiles/tag-and-vendor matching, and
malformed-spec rejection) and its option registry.

Live DHCP address assignment is not attempted. dnsmasq's Linux DHCP server opens an
`AF_PACKET`/`SOCK_RAW` frame socket at `dhcp_init` (to place DHCP replies on the wire for
clients that have no IP yet), and a DHCP client (`busybox udhcpc`) sends its `DHCPDISCOVER`
the same way; the StarryOS packet-socket path is an ARP-only stub with no real frame TX/RX,
so a lease cannot round-trip on this kernel. This is a kernel-side gap, not a config
limitation - the full DHCP option/range surface is validated through the parser above.

DNSSEC/DBus/UBus/Lua/nftset behaviours beyond the compiled feature set are out of scope (see
`--version` for the exact Alpine build).

## Provisioning

On-target `apk add dnsmasq tftp-hpa` against the branch that matches `/etc/alpine-release`,
so apk resolves the current versions with no committed binary and no pinned, drifting URL.
See `programs/SOURCES.md`. The gate skips apk when both binaries are already present (staged
overlay or host chroot pre-flight), so the identical script validates on the host before QEMU.

## Layout

- `prebuild.sh` - stages the dnsmasq binary + tftp-hpa client + gate and grows the per-app rootfs.
- `programs/run-dnsmasq.sh` - the on-target carpet gate.
- `programs/SOURCES.md` - package provenance.
- `build-<target>.toml` x4 / `qemu-<arch>.toml` x4 - per-arch build features and QEMU boot
  (loongarch64 is dynamic-platform with `ax-driver/serial`).
