# Guest IP-link validation

The host-side deterministic check compiles the POSIX Linux endpoint, drops its
first TCP connection, and verifies that the second connection succeeds with a
valid GIPC status frame and metrics:

```bash
python3 scripts/test/guest-ip-link/test_linux_client.py
```

For a captured guest log, validate the required success markers and positive
latency/throughput values with:

```bash
python3 scripts/test/guest-ip-link/verify_metrics.py <guest.log>
```

The full QEMU flow is documented in
`docs/design/starry-rtos-ip-link.md` and requires a Linux rootfs image and the
ArceOS target toolchain. It injects `gipc-linux-client` into the rootfs by
default, then the operator runs it from the Linux shell after the RTOS service
prints `GIPC_RTOS_READY`.
