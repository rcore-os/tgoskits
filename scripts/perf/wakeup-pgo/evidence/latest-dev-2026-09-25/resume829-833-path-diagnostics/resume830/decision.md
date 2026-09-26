# resume830: classify ktimer-worker switch entry

Source `69a33650763538692fafea27c869870ed0313642` on `dev@05175ca38823b631a73777b0130226ddfa558439`. This was a `qperf-metrics`-only probe, not a runtime optimization. `cargo fmt`, all six `cargo xtask clippy --package ax-task` configurations and `cargo xtask starry build -c resume777-preempt-leaves/build.toml` succeeded. Image SHA-256 `bfc70a0df2ea0d3c1991b9ac81474f9dbb7e76535fdab3e087f15ac8da27b21e`; frozen benchmark SHA-256 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`.

OrangePi-5-Plus-2 session `24c41e9d-9722-4d15-8487-1bc01d4a21f9` was released. Four focused timer rounds (FIFO 1/2, OTHER 1/2) each produced 10000/10000 samples with no `not_parked` or missed deadlines. Instrumented p50 values were FIFO 32959/32875 ns, OTHER 66500/65875 ns; they are not native full20 acceptance values. The raw `results.json` SHA-256 is `af215af163ce53cc64c8fecd742752026e686bd43247154fadf26ac195cd9c67`; full serial SHA-256 is `1cb2ba13eb323bcc10ced77dc4ce6c01512a2c31c10b5fffba03b2944ed90d44`.

Actual CPU1 switches **into** the ktimer worker by `RuntimeSchedulerEntry` (Task, PreemptExit, IrqReturn, IrqGuardExit, IrqReturnContinuation) were:

| Round | Publish/claim matched | Switch-in by entry | Switch-in from idle by entry |
| --- | ---: | --- | --- |
| FIFO-1 | 482 | 21, 471, 0, 0, 0 | 0, 471, 0, 0, 0 |
| FIFO-2 | 489 | 25, 480, 1, 0, 0 | 0, 480, 0, 0, 0 |
| OTHER-1 | 11470 | 4, 11394, 53, 0, 0 | 0, 11388, 28, 0, 0 |
| OTHER-2 | 11458 | 11, 11393, 39, 0, 0 | 2, 11379, 24, 0, 0 |

This independently supports the `resume829` coverage correction: the dominant ktimer worker switch is **PreemptExit from idle**, not an `IrqReturn` first-frame switch. The worker is in fact switched in roughly once per matched OTHER claim, so a theory that the worker was already running for most claims is inconsistent with these aggregate counts. Aggregate counters are not per-claim paired; do not infer exact one-to-one identity or the cost of `PreemptExit` from them. FIFO ktimer activity is largely background soft timers, because benchmark FIFO timeouts use hard park. The idle wait retains a preemption guard across WFI and drops it after tick restart; that final task-context exit can become `PreemptExit`. This identifies the next timing boundary but does not show redundant work or a removable multi-microsecond cost.

The exact temporary patch is `probe.patch.gz` (decompress before applying); it was reversed after archiving. No production runtime logic is retained and no 90% acceptance evidence changed. G1/G2 remain the latest valid uninstrumented full20 at 11/20, worst 58.00%.
