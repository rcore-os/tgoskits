# resume864: same-CPU futex wake-window switch classification

Control source: `b292a098bb60ef604e7677c37cd95d926ff08200` on
`dev@714accd8f636c540b2c3554b0b1e5cb885be42a4`. Temporary
`qperf-metrics`-only diagnostics, no PGO, cpufreq feature off. Frozen
benchmark SHA256: `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`.

Hypothesis: in OTHER same-CPU thread futex, the dominant gate wake either
switches the sender before `wake_batch` returns or leaves a Lazy request for
the syscall's user-return path. The earlier aggregate switch count cannot
distinguish these. Read the existing qperf global context-switch and
preempted-switch counters immediately before/after each selected-one,
enqueued-one `wake_batch`. Classify the private key's page offset: the
benchmark's mmap-aligned `handoff_state` has `gate` at 16 and `done` at 20.
Record other keys separately. These offsets must be confirmed against the
compiled benchmark or from source layout; nonmatching counts invalidate
the intended interpretation. No futex or scheduler transition may change.

Run one OrangePi-5-Plus-1 boot with five separate 20000-attempt processes
in fixed OTHER/FIFO/OTHER/FIFO/OTHER order. A round is valid only with
20000/20000 samples, zero not_parked/missed, exit zero, fixed case/policy,
the frozen benchmark SHA, the image SHA and the U-Boot PLL check. Record
before/after debugfs snapshots, full serial and process logs. Confirm
roughly 20000 selected-one gate wakes per round; record done and other
counts rather than silently discarding them. The global counter may include
other CPUs, so FIFO/done rates bound background contamination; a positive
gate delta is not by itself proof of a CPU0 sender switch. A near-zero
OTHER gate fraction rules out frequent counted switches inside the window;
a high fraction with low FIFO background motivates a per-CPU or task-paired
confirmation. An intermediate or noisy result is inconclusive.

This qperf image is diagnostic only. Its p50 and counter read overhead
must not be compared with no-probe full20, Linux RT, or resume862 p50.
No native performance gain can be claimed from this experiment.
