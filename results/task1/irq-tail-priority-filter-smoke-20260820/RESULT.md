# FP-RR IRQ-tail priority-filter smoke

- topology: Linux vCPU0 -> pCPU1, Linux vCPU1 -> pCPU2, Zephyr vCPU0 -> pCPU1
- scheduler: `fp-rr-scheduler`
- duration: 30 s guest workload; 3000 Zephyr samples
- dedicated pCPU: none; host burner: disabled
- runner result: `accepted`; QMP exit status: 0

Linux cyclictest:

```text
avg=2170 us
p99=19549 us (histogram-censored)
max=349196 us
samples=1228 (1210 buckets + 18 overflows)
```

Zephyr:

```text
mean=2.077254 ms
p99=35.631984 ms
p99.9=118.595088 ms
max=145.724576 ms
```

Required closure markers are present and ordered:

```text
PERIODIC LATENCY COMPLETE samples=3000
RT_CYCLICTEST_COMPLETE
RT_CYCLICTEST_HOLD_READY
RT_CYCLICTEST_RELEASED
PSCI_SYSTEM_OFF
```

Raw files, VM-exit snapshots and SHA256 list are in this directory. The run
proves the shared-core control flow and absence of the old Linux-stall failure;
its TCG tail values are not claimed as universal improvements.
