# Physical 2-vCPU Linux idle/stress matrix

## Topology and SMP proof

AxVisor runs with `fp-rr-scheduler`. Linux VM1 has two vCPUs mapped to pCPU2
and pCPU3; RT-Thread VM2 has one vCPU mapped to pCPU1. Linux and RT-Thread host
priorities are 89 and 90 respectively.

The serial record proves Linux SMP bring-up independently of a cosmetic VM
state count:

* `PSCI_CPU_ON target=0x1` is accepted by AxVisor;
* Linux prints `RT_CPUS total=2`;
* `vm list` identifies vCPUs `0,1` for VM1;
* per-pCPU vmexit statistics show activity on pCPU2 and pCPU3;
* the guest's SCHED_FIFO permission probe prints `RT_SCHED_PROBE_OK`.

In stress mode, the stress-ng process and its workers are constrained with
`taskset -c 0`; cyclictest requests affinity CPU1 and priority 90. The kernel
reports that `NO_HZ_FULL` is not compiled in, so the evidence relies on actual
affinity and SCHED_FIFO rather than claiming full-nohz isolation.

## Linux cyclictest

| Mode | samples | min us | avg us | P99 us | P99.9 us | max us | overflow |
|---|---:|---:|---:|---:|---:|---:|---:|
| idle | 20,000 | 58 | 191 | 206 | 313 | 335 | 0 |
| stress | 20,000 | 69 | 192 | 206 | 314 | 513 | 0 |

## Concurrent RT-Thread probe

| Mode | samples | mean us | P99 us | max us | >1 ms |
|---|---:|---:|---:|---:|---:|
| idle | 300 | 307.668 | 490.958 | 529.458 | 0 |
| stress | 300 | 308.341 | 490.375 | 526.541 | 0 |

All rows come from archived raw console logs. Linux histogram CSV/summary files
are next to each log; RT-Thread rows are also present under `derived/`.

