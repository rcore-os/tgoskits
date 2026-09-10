# StarryOS guest IP client

`linux-client.c` is the POSIX StarryOS endpoint for the GIPC control path. It opens a
TCP connection to the ArceOS guest at `10.0.42.2:4242`, sends one versioned
`CONTROL` frame, validates the `STATUS` response and CRC, and retries the whole
request up to three times after a connect, send, receive, or protocol failure.

The source is intentionally freestanding so it can be copied into a Starry or
StarryOS rootfs by the image-preparation step. Build it for the StarryOS guest
with:

```sh
cc -std=c11 -Wall -Wextra -O2 linux-client.c -o /usr/bin/gipc-starry-client
/usr/bin/gipc-starry-client 10.0.42.2
```

The successful run prints `GIPC_STARRY_STATUS` and `GIPC_STARRY_METRIC`; a failed
run prints `GIPC_STARRY_TIMEOUT` or a protocol error and exits non-zero.

The QEMU runner injects `gipc-autostart.sh` into `/etc/profile.d` by default,
so the client runs once when the StarryOS guest opens a shell. Set
`GIPC_AUTORUN=0` to disable this behavior and run the client manually.
