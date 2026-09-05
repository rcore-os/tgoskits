# AICP host reference service

This directory contains the host-side C codec, control-service test fixtures,
and reference TCP service for the AICP v1 demonstration. The service listens
on `0.0.0.0:8800` by default and emits
`AICP RTOS reference server listening on 0.0.0.0:<port>` after bind succeeds.

Build and run the full host validation, including a real process smoke test:

```sh
make -C apps/ai-rtos-demo test
```

The smoke test starts the reference service on an isolated loopback port,
uses the public C client to complete `HELLO`/`STATUS`, verifies an unsupported
message receives `ERROR_BAD_TYPE`, verifies that a drip-fed partial frame
cannot block the next client, then terminates the service. Success is marked
by `AICP_REFERENCE_SERVER_SMOKE_PASSED`.

For manual inspection, build and start the service (the optional argument
selects its TCP port):

```sh
make -C apps/ai-rtos-demo reference-server
apps/ai-rtos-demo/build/aicp_reference_server 8800
```

The service applies a one-second absolute deadline to each received frame and
resets that deadline only after a complete valid frame. Closing the client or
exceeding the deadline ends that session; the listener then accepts the next
connection.
