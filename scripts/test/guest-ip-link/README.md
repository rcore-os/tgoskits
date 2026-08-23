# Guest IP-link validation

The host-side deterministic check compiles the POSIX StarryOS endpoint, drops its
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

For a long-running log containing multiple client requests, compute success
rate, timeout count, P50/P95 request latency, and average effective throughput:

```bash
python3 scripts/test/guest-ip-link/aggregate_metrics.py <guest.log>
```

The full QEMU flow is documented in
`docs/design/starry-rtos-ip-link.md` and requires a StarryOS rootfs image and the
ArceOS target toolchain. It injects `gipc-starry-client` into the rootfs by
default, then the operator runs it from the StarryOS shell after the ArceOS service
prints `GIPC_RTOS_READY`.
