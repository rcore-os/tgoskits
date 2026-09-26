# resume809: frozen Linux RT whole-case PMU comparison

The frozen Linux v7.1 `PREEMPT_RT=y`, `CONFIG_CPU_FREQ=n`, `HZ=1000` image (SHA256 `aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b`) booted on `OrangePi-5-Plus-1` with the PLL checked at the 816 MHz setting. A one-shot initramfs contained the same static benchmark and fixed-slot PMU collector used in resume808, with SHA256 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773` and `47b89a6df65277ef7e0b3c26f6dc1c2a49d019219df7a92fbad2b39b135d1394`, respectively. No Linux or Starry source was changed.

Three OTHER and three FIFO `thread_futex_same_cpu` runs all exited zero with 20000/20000 samples, zero `not_parked`/`missed_deadlines`, four unique CPU0 EL1 PMU events, `enabled_ns == running_ns > 0`, and counts below the configured overflow period. The board session was deleted and API GET returned 404.

| Policy | Image | Diagnostic p50 ns | Instructions/attempt | CPI | L1I refill/1000 instructions | L1D refill/1000 instructions |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| OTHER | Linux RT | 8167 | 15429 | 1.449 | 24.40 | 0.76 |
| OTHER | Starry F | 16479.5 | 16761 | 1.589 | 40.08 | 3.86 |
| FIFO | Linux RT | 8458 | 12096 | 1.359 | 15.38 | 1.25 |
| FIFO | Starry F | 11667 | 15577 | 1.368 | 20.20 | 5.62 |

For OTHER, Starry F/Linux RT ratios are 2.02 for diagnostic p50 but only 1.09 for whole-case instructions/attempt and 1.10 for CPI. L1I refill rate is 1.64 and L1D refill rate 5.05, but these rates are **not** attributed exclusively to the measured forward wakeup. The collector includes child startup, reverse handoffs and background CPU0 kernel work; Linux initramfs and Starry userspace service backgrounds differ. Counts are sequential rather than one atomic read group. Therefore the aggregate metrics cannot explain the full p50 ratio or justify a specific layout/algorithm change. The Linux diagnostic p50 is not a new frozen baseline, and none of these runs enter full20 acceptance.

Decision: stop using whole-case PMU or overflow-PC sampling as a proxy for the forward wakeup. Next experiment should take one CPU0 PMU event at a time, with sender snapshot before the timestamp and receiver snapshot after its first clock read, using the same diagnostic benchmark on both kernels. Calibrate the end-read syscall cost and retain raw per-sample validity; only after a reproducible window-specific difference should source/IR attribution select a candidate. `python3 analyze.py` reproduces this table from `boot.log` and resume808's validated raw records.
