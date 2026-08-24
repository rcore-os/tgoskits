# RR IRQ-tail completion smoke

- topology: Linux vCPU0 -> pCPU1, Linux vCPU1 -> pCPU2, Zephyr vCPU0 -> pCPU1
- scheduler: `rr-scheduler`
- duration: 30 s guest workload; 3000 Zephyr samples
- dedicated pCPU: none; host burner: disabled
- runner result: `accepted`; QMP exit status: 0

Linux cyclictest:

```text
avg=1961 us
p99=7142 us
max=310639 us
samples=1600 (1597 buckets + 3 overflows)
```

Zephyr:

```text
mean=2.715848 ms
p99=31.581792 ms
p99.9=62.545520 ms
max=67.259216 ms
```

Required closure markers are present and ordered:

```text
PERIODIC LATENCY COMPLETE samples=3000
RT_CYCLICTEST_COMPLETE
RT_CYCLICTEST_HOLD_READY
RT_CYCLICTEST_RELEASED
PSCI_SYSTEM_OFF
```

Raw files, VM-exit snapshots and SHA256 list are in this directory. This is a
same-protocol RR control arm, not a static-partition measurement.
