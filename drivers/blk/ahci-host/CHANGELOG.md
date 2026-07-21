# Changelog

## 0.1.0

- Add the rdif-block v0.13 two-phase activation boundary. Runtime planning sees
  one shared controller/IRQ ownership domain with a fixed queue for every PI
  port, while active disks and empty ports are published as `Exact` and
  `Unrouted` selectors respectively.
- Add a fixed, generation-checked IRQ evidence ledger. Shared IRQ facts coalesce
  under one opaque identity, all affected ports are classified error-first, and
  only fully drained evidence can be rearmed.
- Keep initialization completion behind the same linear evidence boundary:
  timer/deadline calls cannot consume IDENTIFY snapshots captured by an IRQ.
- Split owner-thread domain service from controller activation and retain all
  partially converted queues/ports in typed publication failures.

- Add mask-only AHCI BAR discovery and an explicit, absolute-deadline
  initialization state machine for firmware handoff, HBA reset, COMRESET, link
  activation, engine startup, and ATA IDENTIFY. Discovery clears only GHC.IE
  and implemented-port PxIE before any OS action is enabled.
- Separate shared `AhciHost` ownership from per-disk `AhciPortDevice` views.
  Each physical ATA port retains independent geometry, queue limits, request
  generations, and one serialized owned-request queue; ports are never treated
  as interchangeable queues of one logical block device.
- Add a fixed 64-entry snapshot ring per port and one shared destructive IRQ
  endpoint that acknowledges error-first register snapshots and routes work by
  physical port ID without allocating or blocking.
- Keep capture-gate contention in the level-triggered hard-IRQ path and expose
  no task-side deferred acknowledgement during lifecycle recovery.
- Require acknowledged IRQ evidence for normal I/O completion. Absolute
  watchdog expiry enters controller recovery and never polls completion state.
- Add typed controller-wide DMA quiescence, reinitialization, and fail-closed
  quarantine paths for queue recovery and exclusive ownership handoff.
- Serialize active-request publication and the PxCI doorbell with the unique
  destructive IRQ register window, so an old latched FIS cannot complete a
  request between publication and hardware submission.
- Bind DMA reclaim to the controller cookie and a proof epoch newer than the
  queue publication epoch, rejecting stale or repeated lifecycle proofs.
- Keep fallible queue shutdown separate from request ownership publication;
  accepted AHCI buffers return only through IRQ completion or proof-gated DMA
  reclaim.
- Track initialization and normal-I/O IRQ endpoint liveness separately, reject
  controller unmasking after an endpoint is dropped, and keep the first hardware
  command behind a live initialization endpoint rather than a historical
  `take_irq_handler` flag.
- Require the initialization endpoint to be released before extracting the
  normal-I/O endpoint, and serialize IDENTIFY publication with the same
  destructive-register gate used by ordinary submissions.
- Classify every queued IRQ error before publishing completion, both for normal
  requests and ATA IDENTIFY, while preserving completion candidates across
  bounded continuations.
- Reject controller recovery epochs that are not strictly newer than the
  initially published or most recently consumed epoch before touching MMIO.
