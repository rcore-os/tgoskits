# Sources

- `tun-echo.c` - original work for this carpet. A self-contained `/dev/net/tun`
  probe: creates and configures a layer-3 `tun0`, then drives the ICMP echo
  datapath in both directions and validates the results. No third-party code.
- `run-tun-tap.sh` - original driver script; prints `TEST PASSED`/`TEST FAILED`.

The ioctl surface exercised (`TUNSETIFF`, `TUNGETIFF`, `TUNGETFEATURES`,
`SIOCSIFADDR`, `SIOCSIFNETMASK`, `SIOCSIFFLAGS`) mirrors Linux
`Documentation/networking/tuntap.rst` and `drivers/net/tun.c`, and is the same
surface probed by LTP `testcases/kernel/syscalls/ioctl/ioctl03.c`
(`TUNGETFEATURES`) and `uevents/uevent02.c` (open `/dev/net/tun` + `TUNSETIFF`).
