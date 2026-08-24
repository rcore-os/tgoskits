# Hybrid-topology latency capture

These host-side tools reproduce the quiet-window Zephyr latency measurement in
the final StarryOS/Zephyr hybrid topology. They are not board runtime
dependencies. The host requires Python 3 and `pyserial`; the board console is
expected at 1,500,000 baud.

The Zephyr application uses `qemu_cortex_a53` as its build board. QEMU's native
architectural counter is 62.5 MHz, while the RK3588 counter exposed to the guest
is 24 MHz. ATK board builds therefore must set:

```bash
TASK2_TIMER_FREQUENCY_HZ=24000000
```

Generic QEMU builds leave the variable unset and retain 62.5 MHz. The effective
frequency is validated during the build, written to `manifest.toml`, printed by
the guest, and checked again by the analyzer.

Start an idle 30,000-sample capture after RAM-booting the matching FIT:

```bash
sudo -n python3 scripts/test/rt-partition/run-hybrid-latency.py \
  results/rr-idle.log --samples 30000 --idle
```

For stress, omit `--idle`. The runner first requires a live
`RKNN_CONTROL_EVENT -> TASK3_RKNN_INPUT -> TASK3_STATUS_RECEIVED` chain, or a
saved causal-evidence log containing at least three of each record.

Analyze an idle capture without repairing or filtering the raw UART data:

```bash
python3 scripts/test/rt-partition/analyze-hybrid-latency.py \
  results/rr-idle.log --samples 30000 --idle \
  --output results/rr-idle-analysis
```

The analyzer rejects a non-24-MHz board marker, missing or interleaved CSV rows,
non-contiguous sequence numbers, deadlines that are not exactly 10 ms apart,
inconsistent jitter arithmetic, UART output inside the sampling window, and
workload activity that does not match the idle/stress label. For a QEMU capture,
pass its configured frequency with `--timer-frequency-hz`. The analyzer writes
the validated full CSV, `summary.txt`, and `spikes-over-3ms.csv`. The runner
reports sampling and CSV-export wall times separately; only the former belongs
to the real-time experiment.
