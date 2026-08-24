# Zephyr software-vIRQ consumer

This is the common guest workload for the OpenRace software-vIRQ
comparison. It runs with one vCPU, installs ISRs for virtual interrupts 48
and 49, and records the guest-side timestamp of each interrupt. Two host
injectors drive the two vectors concurrently from different host CPUs; the
injector is deliberately outside this directory and is identical across
variants.

Build the raw image with the same Zephyr revision/toolchain as
`zephyr-periodic`:

```bash
west build -p always -b qemu_cortex_a53 \
  scripts/test/zephyr-soft-virq -d /tmp/zephyr-soft-virq-build
```

Use `zephyr.bin` (not the ELF) in
`axvisor-qemu-aarch64.toml`, then build AxVisor with the experiment feature:

```bash
FEATURES=openrace-realtime cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-aarch64.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-aarch64.toml \
  --vmconfigs scripts/test/zephyr-soft-virq/axvisor-qemu-aarch64.toml
```

The run is valid only when the log contains both:

```text
SOFTWARE VIRQ COMPLETE streams=2 samples_each=300 total=600
VIRQ_INJECT_COMPLETE ... errors=0
```

The host injector uses the same two-second guest warm-up for every variant.
The warm-up starts after the VM enters the running state and is not part of
the injection-to-ISR latency samples; it prevents boot-time interrupts from
preceding the guest's `SOFTWARE VIRQ READY` marker.

Save the complete serial log. `VIRQ_INJECT` lines are the host-side request
timestamps; the CSV lines are guest ISR timestamps in
`vector,sequence,timestamp_ns` form. `VIRQ_TRACE` lines are the bounded
per-host-CPU delivery trace.
