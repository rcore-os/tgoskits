# Linux/Zephyr Direct Shared-pCPU A/B

This experiment isolates the scheduler feature while keeping the guest
topology constant. It is the first direct共核 cell for Task 1: Linux vCPU0 and
Zephyr vCPU0 both run on pCPU1; Linux vCPU1 runs on pCPU2. No dedicated CPU,
host burner, or passthrough Zephyr path is used.

## Protocol

- AxVisor commit: `9b5edcef3eee16280f9483d7da9072918149b0a9`
- QEMU: AArch64 TCG, `-smp 4`, 8 GiB RAM
- RR arm: `board-qemu-aarch64-rr.toml`
- Fixed arm: `board-qemu-aarch64-rt.toml`
- Three interleaved runs: `rr, fixed, fixed, rr, rr, fixed`
- Linux cyclictest window: 60 s guest uptime
- Zephyr samples: 3000 per run
- All six runs passed the acceptance markers and histogram checks

Raw artifacts:

- [comparison.txt](guest-shared-ab-3x-20260819/comparison.txt)
- [protocol.txt](guest-shared-ab-3x-20260819/protocol.txt)
- [RR run 01 metadata](guest-shared-ab-3x-20260819/rr/run-01/stress-guest-shared/meta.txt)
- [Fixed run 01 metadata](guest-shared-ab-3x-20260819/fixed/run-01/stress-guest-shared/meta.txt)

## Results

Values below are medians across three runs. Improvement is baseline RR divided
by Fixed; values below 1 mean Fixed is worse.

| Metric | RR median | Fixed median | Fixed change |
| --- | ---: | ---: | ---: |
| Zephyr P99 jitter | 1.918 ms | 1.767 ms | 8.6% lower |
| Zephyr P99.9 jitter | 2.488 ms | 5.681 ms | 128.4% higher |
| Zephyr max jitter | 3.551 ms | 6.689 ms | 88.3% higher |
| Zephyr tolerance misses | 2326 | 1625 | 30.1% lower |
| Linux P99 latency | 1654 us | 1781 us | 7.7% higher |
| Linux P99.9 latency | 9503 us | 2135 us | 77.5% lower |
| Linux max latency | 2.530 s | 48.595 s | 1821% higher |

The paired P99 ratios reported by `compare-rt-runs.py` are `1.145356` for
Zephyr and `0.883212` for Linux. The single P99 number is therefore not a
stable worst-case improvement: Fixed changes the distribution shape and
produces a much heavier Zephyr/Linux maximum tail in this TCG shared-core cell.

## Attribution

The mapping is visible in every run's `meta.txt` and in the AxVisor serial log:

```text
linux_phys_cpu_ids=1,2
zephyr_phys_cpu_ids=1
VM[1] VCpu[0] ... cpumask: [1, ]
VM[2] VCpu[0] ... cpumask: [1, ]
```

Thus the result is not the earlier static partition effect. It is a software
scheduler A/B under direct Linux/Zephyr vCPU contention. It does not claim that
IRQ-tail arbitrary preemption is implemented, nor that Linux P99 is reliably
improved. The next mechanism-level experiment should assign distinct vCPU
priorities (while retaining this shared pCPU mapping) so fixed-priority
preemption can be attributed separately from scheduler-class effects.
