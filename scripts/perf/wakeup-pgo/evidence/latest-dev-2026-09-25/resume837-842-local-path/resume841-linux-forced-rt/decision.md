# resume841: matched-board Linux RT forced-preemption comparison

The frozen Linux RT `Image` SHA-256 is `aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b`; DTB SHA-256 is `316dd15b329756be3887dea22f89fc8d1f5b055f8769761f4144b6b1caaea994`. A one-shot initramfs (SHA-256 `cfd660201bb19be461395680510845358359df510adaf34775e5d39c5b29bbd4`) carried the **same three user binaries** used by Starry `resume840 run3`, with no PMU reads or kernel probe. On OrangePi-5-Plus-2, the session power-cycled, verified the fixed U-Boot PLL values and Linux `PREEMPT_RT` banner, ran eight rows in the identical order, and was released. Boot log SHA-256 is `00166a1157b41312fb3f585c671981ffdfe8b2fe8e455db58c1d7863fc4e45a9`; full serial SHA-256 is `6dec304e0165fbd71a1ad189466347dbf7653e39c476a24371a01506141c42be`.

`python3 analyze.py` checks source/image/benchmark hashes, run order, result lines and sample validity against the archived raw logs. Linux `control OTHER` round 2 had 19999/20000 samples and `not_parked=1`, despite benchmark process exit 0. It is invalid and excluded from OTHER summaries; all seven other Linux rows are valid. All eight Starry rows from `resume840 run3` are valid.

| Binary/policy | Starry p50 rounds (ns) | Linux RT p50 rounds (ns) | RT/Starry median |
| --- | ---: | ---: | ---: |
| rebuilt control FIFO | 11958, 11958 | 8750, 8459 | 71.96% |
| forced FIFO | 10792, 10792 | 6417, 6708 | 60.81% |
| frozen FIFO | 11958, 12250 | 8459, 8458 | 69.88% |
| rebuilt control OTHER | 14584, 14875 | 8458, invalid | no paired median |

The higher-priority receiver helps **both** kernels; it reduces the Starry equal-priority median by 1166 ns and the Linux RT median by 2042 ns. In this user-level condition, the relative gap grows under forced local immediate preemption rather than disappearing. That contradicts the idea that Fair-specific selection alone accounts for the same-CPU deficit. It points to the shared immediate-wake/preemption/switch transaction as the next source-audit target, but does not isolate an individual function or establish an attainable native gain. Different priority ordering is a different workload; this table cannot replace the frozen Linux RT full20 comparison. Frequency was not measured in the eight individual windows, though the same cpufreq-off G and frozen Linux RT configurations have independent near-816 MHz probes. No kernel edit, full20 candidate or acceptance result was produced.
