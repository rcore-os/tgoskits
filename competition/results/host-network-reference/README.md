# Host UDP fault-injection reference

This is the host-side deterministic reliability gate. It proves the same wire
codec and endpoint state machine used by the guest applications before QEMU is
involved. Its full-loop timer includes neural inference, serialization,
transport, and the host plant response, but remains host-only. The QEMU
Linux-to-Zephyr result is recorded separately.

Regenerate with:

```text
bash competition/ivc/run-host-loopback.sh \
  competition/results/host-network-reference
```
