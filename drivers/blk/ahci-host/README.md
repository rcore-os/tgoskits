# ahci-host

`ahci-host` is a `no_std` AHCI controller core for interrupt-driven block I/O.
It contains hardware state machines and owned-request queues, while leaving PCI
or FDT discovery, MMIO mapping policy, IRQ registration, timers, workers, and
thread wakeups to the consuming kernel runtime.

The public lifecycle has four strict boundaries:

1. `AhciHost::discover` maps the supplied BAR, clears global interrupt enable,
   reads PI, and clears PxIE only for implemented ports. It does not reset the
   HBA, issue ATA commands, ring a queue doorbell, or acknowledge completion
   status.
2. The runtime binds the initial IRQ endpoint, enables IRQ delivery, and drives
   `ControllerInitEndpoint` with monotonic time and captured IRQ events. Firmware
   handoff, HBA reset, COMRESET, link activation, and IDENTIFY are bounded states
   with absolute deadlines. Dropping the move-only endpoint invalidates that
   activation permit; neither initialization nor normal queue activation may
   unmask the controller without the matching live endpoint. The initial
   endpoint must be released before the normal-I/O endpoint can be extracted,
   preserving one destructive status owner throughout the transition.
3. `AhciHost::take_port_device` extracts one `AhciPortDevice` for each identified
   ATA disk. Every device view has its own geometry, limits, port ID, request
   generation, and single serialized queue. AHCI ports are separate disks and
   are never presented as interchangeable hctx queues of one block device.
   The shared hard-IRQ endpoint is the sole normal-I/O reader and W1C owner for
   destructive status and fans stable snapshots out by physical port ID. A
   watchdog can fail a request and start recovery, but never probes hardware for
   a missed completion. The worker classifies all already-published IRQ errors
   before returning success, including across bounded ring continuations.
4. Shutdown, recovery, and passthrough first stop both AHCI DMA engines and reset
   the HBA. Request and DMA backing ownership is returned only after a matching
   `DmaQuiesced` proof from a strictly newer controller epoch; an unproven
   engine is quarantined fail-closed.

Each port uses one fixed-capacity IRQ snapshot ring (64 entries) and admits one
active command. The rdif-block v0.13 activation path publishes exactly one
controller/shared-IRQ ownership domain containing every implemented physical
port. Identified disks use an exact logical-device selector; implemented but
empty ports remain explicit `Unrouted` queues so sparse PI topology does not
change after activation.

The consuming runtime must move that domain into one CPU-pinned maintenance
session. Only its owner thread may initialize the controller, submit commands,
consume evidence, recover, or quiesce DMA. The hard-IRQ endpoint acknowledges
stable controller/port facts into a fixed evidence ledger and returns one opaque
`IrqEvidenceId`; it never services queues or chooses OS retry policy. Repeated
captures coalesce under the same linear identity until every affected port is
drained. A retained identity cannot yield a rearm permit, and stale evidence or
mask generations fail closed.
