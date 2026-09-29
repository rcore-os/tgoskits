# resume808: current-source whole-case PMU diagnostic

## Identity and validity

On `OrangePi-5-Plus-1`, the same-source ordinary A image (`750589429f5afe81b16775dde50050fedb936614b78d7bbde999a51856618473`) and PGO F image (`e48394c7ef55008d0badf2cd6dc841fa8fa6593e069720304e2400634614fd6d`) were run A1-F1-F2-A2. Source was `dev@05175ca388` plus the identical `memset` fix and temporary exporter feature on both sides. Frozen benchmark SHA256 was `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`; fixed-slot PMU collector SHA256 was `47b89a6df65277ef7e0b3c26f6dc1c2a49d019219df7a92fbad2b39b135d1394`.

Each boot ran three OTHER and three FIFO `thread_futex_same_cpu` focused diagnostics. All 24 collector invocations exited zero and reported four unique CPU0 EL1 hardware events with `enabled_ns == running_ns > 0`, counts below the configured 32-bit overflow period, and one benchmark result. Three OTHER rounds nevertheless had `19999/20000` samples and `not_parked=1` (F1 rounds 2-3, A2 round 3); these remain in the raw logs and are **excluded** from the grouped results. Other rounds were valid. No source or production benchmark was changed, and the board session was deleted (API GET returned 404).

## Group medians, diagnostic only

| Image | Policy | Valid runs | Diagnostic p50 ns | Instructions/attempt | CPI | L1I refill/1000 instructions | L1D refill/1000 instructions |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| A | OTHER | 5 | 27417 | 21127 | 2.024 | 65.57 | 3.15 |
| A | FIFO | 6 | 19833 | 19986 | 1.796 | 56.55 | 3.68 |
| F | OTHER | 4 | 16479.5 | 16761 | 1.589 | 40.08 | 3.86 |
| F | FIFO | 6 | 11667 | 15577 | 1.368 | 20.20 | 5.62 |

PGO reduces whole-case retired instructions and instruction-cache refills substantially on both policies. F still shows higher OTHER than FIFO instruction count, CPI, and L1I refill rate. The collector includes child startup, reverse handoffs and background CPU0 kernel activity, and counts events sequentially rather than as one atomic group. Therefore neither the grouped diagnostic p50 nor these PMU ratios are an exclusive forward-wakeup cost, a Linux RT comparison, or a new full20 acceptance run. They do not justify repeating global outlining, machine-function splitting or ExtTSP, all previously rejected. No optimization was retained.

The next useful comparison is the same collector and benchmark on the frozen Linux RT kernel at the verified ~816 MHz board setting, followed by a specific source/IR attribution if the difference persists. Raw logs, hashes, run order and invalid rounds are preserved in this directory; `python3 analyze.py` recomputes the table.
