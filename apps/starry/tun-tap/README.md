# tun-tap - StarryOS `/dev/net/tun` TUN+TAP datapath and lifecycle carpet

Exercises both the layer-3 **TUN** (`IFF_TUN | IFF_NO_PI`) and layer-2 **TAP**
(`IFF_TAP | IFF_NO_PI`) interfaces on all four architectures. Both probes are
self-contained static-musl C programs; they need no guest network, no second
endpoint, and no package fetch at runtime.

Scope: this carpet covers end-to-end traffic and device lifecycle. Exact ioctl
return values, flag semantics and error codes are covered by the system test
`test-suit/starryos/qemu/system/bugfix-bug-tun-tap-abi`.

## TUN (layer 3) - `programs/tun-echo.c`

1. `/dev/net/tun` character device exists.
2. `TUNGETFEATURES` includes `IFF_TUN`.
3. `TUNSETIFF` + `TUNGETIFF` round-trip on a layer-3 `tun0`.
4. `SIOCSIFADDR` / `SIOCSIFNETMASK` / `SIOCSIFFLAGS` configure `10.8.0.1/24` UP.
5. Ingress->egress ICMP datapath: inject an echo *request* framed from `10.8.0.2`
   to `10.8.0.1`; the kernel stack generates an echo *reply* and routes it back
   out `tun0`; validate source/dest swap, id/seq echo, IPv4 and ICMP checksums.
6. Egress observation: a UDP datagram sent to another host on the `tun0` subnet
   appears on the fd.

## TAP (layer 2) - `programs/tap-carpet.c`

1. `TUNSETIFF` creates `tcarp0` with `IFF_TAP | IFF_NO_PI`.
2. `TUNGETIFF` echoes the name and flags (`IFF_TAP | IFF_NO_PI`).
3. `SIOCGIFHWADDR` - the kernel-assigned MAC is non-zero, unicast, and locally
   administered.
4. Configure `10.8.2.1/24` UP via `SIOCSIFADDR`/`SIOCSIFNETMASK`/`SIOCSIFFLAGS`.
5. ARP handshake (L2 framing): write an ARP-request Ethernet broadcast frame
   (who-has `10.8.2.1` tell `10.8.2.2`); read back the ARP reply and validate
   Ethernet dst/src, ARP operation, sender/target MAC and IP fields.
6. Close/recreate lifecycle: a non-persistent TAP device is gone (`ENODEV`) after
   its last fd closes, and the same name can be created again.
7. `TUNSETPERSIST` lifecycle: a persistent device survives its last fd close and
   keeps its MAC across re-attach, which distinguishes re-attach from re-creation;
   clearing persist removes the device on close (`ENODEV`).
8. Second-fd `EBUSY`: attaching a second fd to a single-queue TAP returns `EBUSY`.

## Running

```
cargo xtask starry app qemu -t tun-tap --arch x86_64
```

The wrapper prints `TEST PASSED` or `TEST FAILED` on a line of its own, which the
QEMU configs match as success and failure.

## Layout

```
prebuild.sh                     cross-compile tun-echo.c + tap-carpet.c (static musl)
programs/tun-echo.c             layer-3 TUN datapath probe
programs/tap-carpet.c           layer-2 TAP framing + lifecycle carpet
programs/run-tun-tap.sh         driver: runs both probes, single verdict line
build-<target>.toml x4          per-arch kernel build
qemu-<arch>.toml x4             per-arch qemu run
```
