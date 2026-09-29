# resume858: timer-worker fresh wake classification

This is a qperf-only diagnostic on source
`69a33650763538692fafea27c869870ed0313642`, based on
`dev@05175ca38823b631a73777b0130226ddfa558439`. The archived
`probe.patch.gz` preserves the generation-paired resume826 probe and a temporary
`IrqWaitCell` detail that distinguishes `WakeResult::Notified` (Fresh) from
`WakeResult::AlreadyPending`. It does not change the notification state
machine. The image used no PGO or cpufreq feature and has SHA-256
`ec288c217d771e384601d2ea5b4e3a617e81eeeb199730448d34085a13e1390f`;
frozen benchmark SHA-256 is
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`.

OrangePi-5-Plus-2 session `9ca72968-9df9-48e2-9fbb-147c3ffb808a`
passed the U-Boot PLL check. One boot ran FIFO timer twice, then OTHER timer
twice. Every process produced 10000/10000 samples, zero `not_parked` and
missed deadlines, and exit zero. The script released its session and a
subsequent API lookup returned 404. Raw `results.json` SHA-256 is
`d52526b6c3d674505354da9f1a70b726d42bdecbfe127634b3a18b0a27cd4083`;
serial log SHA-256 is
`9614af47601dbe2780d6fbb177de4adc50ba3b5ac778ed609a96b79248b57231`.

CPU1 hard-IRQ ktimer notification deltas:

| Policy | Round | Fresh | AlreadyPending | Cell Pending | Generation-paired notify/claim | Notify-to-claim p50 bucket |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| FIFO | 1 | 504 | 0 | 0 | 504 | 16-18 us |
| FIFO | 2 | 491 | 0 | 0 | 491 | 16-18 us |
| OTHER | 1 | 11443 | 0 | 16 | 11443 | 16-18 us |
| OTHER | 2 | 11437 | 0 | 6 | 11437 | 16-18 us |

`ktimer_notify_notified == Fresh + AlreadyPending` and all paired notified
events in these rounds were Fresh. The old resume826 `Notified` classification
therefore did **not** hide an AlreadyPending-heavy workload on this image.
The OTHER soft-worker notifications are the relevant timer path; the FIFO
worker notifications are background events because FIFO park uses ParkHard.
The notify-to-claim histogram remains a qperf-inclusive, generation-paired
kernel interval containing IRQ remainder, scheduler frame, context switch and
worker wait return. It is **not** a removable 16-18 us native p50 component
and cannot be added to separate stage medians. Instrumented benchmark p50s
were FIFO 33625/33584 ns and OTHER 66334/66417 ns, not acceptance data.

Decision: diagnostic only, no production change or new native performance
gain. The fresh-wake result rules out coalesced `AlreadyPending` as an
explanation for resume826's dominant interval. Continue only with a
source-specific, semantics-preserving worker-dispatch candidate that retains
PREEMPT_RT task-context timer delivery; the existing resume831 stage split
does not by itself prove such a candidate. `cargo fmt`, ax-task clippy 6/6,
Starry qperf build and `python3 check.py` passed. Starry-kernel full clippy
was interrupted after 8/72 passing configurations because free disk fell
to 118 MB; it is **not** counted as passed. Only the nine incremental
`starry_kernel` session directories created by that check were removed,
restoring about 1.4 GB free. Latest valid uninstrumented G1/G2 full20
remains 11/20 at 90%, worst OTHER same-CPU futex 58.00%; three-boot and
same-source p50/p99/p99.9 gates remain open.
