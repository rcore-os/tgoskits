# wifi_switch — runtime Wi-Fi AP↔STA switch demo

A tiny userspace tool that drives StarryOS's wireless-extensions `ioctl` path
to switch the `wlan0` interface (aic8800 on sg2002) between **SoftAP** and
**Station** at runtime, without a reboot.

It exercises the full control-plane chain added in `feat/wifi-mode-switch`:

```
wifi_switch (SIOCSIW* + COMMIT)
  -> StarryOS socket ioctl  (kernel/src/file/wext.rs)
  -> ax_net::reconfigure_wifi
       -> rd_net::WifiControlHandle -> aic8800 WifiControl  (VIF teardown + switch)
       -> Service::reconfigure_as_{ap,sta}                  (IP / DHCP role)
```

## Build (riscv64, static musl)

Use the same cross toolchain as the other sg2002 rootfs binaries. In the
build container (`tgoskits-sg2002`):

```sh
cd /workspace/apps/starry/picoclaw-cli
/opt/riscv64-linux-musl-cross/bin/riscv64-linux-musl-gcc \
    -static -O2 -Wall -Wextra -o wifi_switch wifi_switch.c
```

Produces a static-PIE riscv64 ELF.

## Install onto the SD card

Drop it into the StarryOS p3 rootfs at `/usr/bin/`, next to `tennis`,
`test_motor`, etc. (see `docs/sd-card-build.md`):

```sh
sudo cp wifi_switch /tmp/sdpart/usr/bin/
sudo chmod +x /tmp/sdpart/usr/bin/wifi_switch
```

## Run on the board

```sh
# Become an open SoftAP (default channel 6). This is the boot default too,
# so use it to switch BACK to AP after testing STA.
wifi_switch ap PicoClaw-Car
wifi_switch ap PicoClaw-Car 11        # explicit channel

# Join an existing network in station mode.
wifi_switch sta MyHomeWifi mypassword # WPA2
wifi_switch sta OpenCafeWifi          # open network
```

## How to tell it worked

The boot default is SoftAP `PicoClaw-Car` on `192.168.50.1/24` with a DHCP
server leasing `192.168.50.2`. A successful switch is observable as:

1. **Kernel log** — each commit logs `wlan0: wifi mode switch complete` from
   `ax_net::reconfigure_wifi`, preceded by the aic8800 link-layer lines
   (`MM_REMOVE_IF`, then either `connected to '<ssid>'` for STA or
   `AP started, APM_START_CFM=...` for AP), and the stack role line
   (`dev N: reconfigured as STA, DHCP client enabled` /
   `... reconfigured as AP 192.168.50.1/24, DHCP server lease 192.168.50.2`).

2. **STA mode** — after `wifi_switch sta <ssid> <pass>`, the interface drops
   its `192.168.50.1` SoftAP address and DHCP-discovers a new one from the
   joined AP. Watch the log for `DHCP acquired address <x>`; then a client on
   that LAN can be reached (e.g. `ping`/`wget` from the board).

3. **AP mode** — after `wifi_switch ap PicoClaw-Car`, an external phone/laptop
   sees the `PicoClaw-Car` SSID again and gets `192.168.50.2` by DHCP; it can
   ping `192.168.50.1`.

4. **Round trip** — `ap -> sta -> ap` should leave you back at a working SoftAP,
   confirming repeated VIF teardown/bring-up is clean (the original goal of the
   `MM_REMOVE_IF` teardown work).

> Note: a non-zero exit with `ioctl 0x8b00 failed` means COMMIT was rejected —
> check that all of mode/ssid (and channel for AP) were staged first, and read
> the kernel log for the `reconfigure_wifi` failure reason.
