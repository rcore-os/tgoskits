# resume810: forward-window PMU comparison (diagnostic only)

The same diagnostic benchmark (SHA256 `7ea6ace6192fb9ffafc2cd0ce13c2c5cc97ceb7eb6808e282c0a417e7ffa82b6`) ran on frozen Linux v7.1 PREEMPT_RT and current-source Starry ordinary A/PGO F images on OrangePi-5-Plus-1. The Starry order was A1-F1-F2-A2. Each run counted one CPU0 EL1 event between the sender timestamp and the receiver's first clock read, then took an adjacent PMU read to estimate end-read overhead. All Starry runs and all but one Linux run had 20000/20000 samples and no `not_parked`; Linux L1D OTHER round 2 had 19999/20000 and `not_parked=1`, was retained in the log and excluded from grouped medians. Both board sessions were deleted and returned 404 on recheck.

| Event, approximate adjusted p50 per attempt | Linux OTHER | Linux FIFO | Starry A OTHER | Starry A FIFO | Starry F OTHER | Starry F FIFO |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Instructions | 4992 | 5305 | 11106 | 8974 | 8889 | 7044 |
| Cycles | 8818.5 | 8114 | 25015 | 17888.5 | 15444 | 10890 |
| L1I refills | 224 | 155 | 815.5 | 608.5 | 456 | 257.5 |
| L1D refills | 57* | 21 | 38.5 | 38 | 49.5 | 48 |

`*` Linux L1D OTHER has only one valid run. These values subtract the median of the adjacent end-read syscall from the raw median; they are **not** exact exclusive source-function counts. The diagnostic benchmark changes cache and pacing, the first read and calibration read have different cache states, and raw per-sample PMU deltas were not exported. Linux and Starry also have different background activity. The OTHER-minus-FIFO difference in Starry F exceeds the same Linux difference by about 2158 instructions, 3849.5 cycles and 129.5 L1I refills. This is only a lead for Fair wake/preemption investigation, not a measured removable cost or a valid full20 improvement.

No production source or frozen benchmark was changed; no optimization was retained. The last valid original full20 remains `resume788` F2: 11/20 meet the 90% threshold, worst OTHER same-CPU futex 16625 ns versus frozen Linux RT 8458 ns (50.88%). `python3 analyze.py` verifies run identity, hashes, sample completeness and the diagnostic medians from raw logs. Linux session `30941b98-0bc7-4f2b-9050-0c0823642877`; Starry session `53aaa887-4880-4285-ae7f-905af7e72960`.
