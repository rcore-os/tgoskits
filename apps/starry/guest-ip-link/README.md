# Starry/Linux guest IP client

`linux-client.c` is the POSIX endpoint for the GIPC control path. It opens a
TCP connection to the RTOS guest at `10.0.42.2:4242`, sends one versioned
`CONTROL` frame, validates the `STATUS` response and CRC, and retries the whole
request up to three times after a connect, send, receive, or protocol failure.

The source is intentionally freestanding so it can be copied into a Starry or
Linux rootfs by the image-preparation step. Build it in a Linux/Starry guest
with:

```sh
cc -std=c11 -Wall -Wextra -O2 linux-client.c -o /usr/bin/gipc-linux-client
/usr/bin/gipc-linux-client 10.0.42.2
```

The successful run prints `GIPC_LINUX_STATUS` and `GIPC_LINUX_METRIC`; a failed
run prints `GIPC_LINUX_TIMEOUT` or a protocol error and exits non-zero.

The QEMU runner injects `gipc-autostart.sh` into `/etc/profile.d` by default,
so the client runs once when the Linux guest opens a shell. Set
`GIPC_AUTORUN=0` to disable this behavior and run the client manually.
