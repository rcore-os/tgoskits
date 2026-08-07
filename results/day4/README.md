# Day4 latency results

These CSV files contain the raw output of the Zephyr 10 ms periodic sampler.
Summarize one file with:

```bash
python3 scripts/test/rt_latency_stats.py results/day4/native.csv
```

## Evidence status

| File | Status | Linux guest evidence |
| --- | --- | --- |
| `native.csv` | valid native QEMU reference | not applicable |
| `axvisor-zephyr-single.csv` | valid AxVisor single-Guest baseline | not applicable |
| `axvisor-dual-idle.csv` | observational only | VM task started, Linux kernel banner absent |
| `axvisor-dual-stress-unverified.csv` | not valid for stress conclusions | stress marker absent |

The dual-Guest files are retained to show the observed AxVisor/Zephyr path,
but they must not be described as Linux-idle or Linux-stress measurements.
