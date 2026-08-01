# AxVisor Linux/Zephyr IVC reference

This directory retains three complete QEMU AArch64 runs of the competition
topology: a two-vCPU Linux controller, a Zephyr v4.3.0 control endpoint, two
virtio-net devices, and AxVisor's isolated Ethernet segment. The application
channel is UDP/IPv4 using the versioned `IVC/1` protocol; no vsock, shared
memory, hypercall, bridge, NAT, or host-facing NIC carries control data.

## Normal closed-loop results

Both policies ran the same 1,800-command, 100 ms thermal scenario and completed
with zero application errors, timeouts, retransmissions, RTOS duplicates, or
RTOS protocol errors.

| Metric | Manual fixed | Neural |
| --- | ---: | ---: |
| Sent / acknowledged | 1,800 / 1,800 | 1,800 / 1,800 |
| Full-loop p50 / p95 / p99 / max | 3,902 / 4,670 / 5,423 / 19,656 us | 3,894 / 4,652 / 5,657 / 20,917 us |
| Pre-send p50 / p95 / p99 / max | 3 / 4 / 8 / 269 us | 13 / 17 / 45 / 376 us |
| Transport p50 / p95 / p99 / max | 3,898 / 4,667 / 5,420 / 19,555 us | 3,880 / 4,632 / 5,645 / 20,622 us |
| Effective throughput | 9.963 msg/s | 9.962 msg/s |
| RMSE | 9,258.906 mC | 5,932.491 mC |
| Integrated absolute error | 1,429,224.700 mC*s | 686,993.400 mC*s |
| Maximum overshoot | 6,840 mC | 13,428 mC |

Neural control reduced RMSE by 35.93% and integrated absolute error by 51.93%,
but increased maximum overshoot. It is therefore better on the two required
aggregate error metrics in this deterministic scenario, not on every control
metric.

`full_loop` starts on Linux before observation construction and policy
inference, and ends after matching STATUS plus ACK decode. It includes policy,
encoding, UDP/IP and virtio transport, the RTOS control/plant step, response
transport, and decode. Linux's single monotonic clock avoids subtracting the
unrelated Linux and Zephyr clock epochs; serialization truncates durations to
one-microsecond resolution.

## Deterministic ACK-loss result

The separate fault image suppresses the first ACK for every fifth fresh
command, while still returning STATUS. The validated retained 100-command
result has:

- 100/100 acknowledged and 100 fresh commands applied exactly once;
- 20 injected ACK losses at sequences 5, 10, ..., 100;
- 20 retransmissions, recoveries, and duplicate receptions;
- 20 duplicate control side effects suppressed;
- 120 STATUS and 100 ACK frames returned; and
- zero ERROR frames, protocol errors, or terminal timeouts.

Its full-loop p50/p95/p99/maximum is
3,953/110,808/111,484/111,548 us. The intentional 100 ms retry delay dominates
the tail. The endpoint also enters zero-actuator safe fallback after controller
silence. Typed ERROR behavior is implemented and covered by Rust and strict C
regression tests; this retained fault run targets retry and exact-once behavior
rather than malformed-frame injection.

## Evidence and integrity

- `*-summary.json` is produced by `competition/ivc/analyze_qemu.py` and binds
  its source log by SHA-256.
- `*-qemu.log.gz` is deterministic gzip of the complete QEMU/build console,
  including both guest boots, ready markers, terminal result, success-regex
  match, safe fallback, and successful `axbuild` completion.
- [`metadata.json`](metadata.json) records source, tools, guest images,
  configurations, UTC intervals, exit codes, retained compressed hashes, and
  uncompressed source-log hashes.

Expected uncompressed log hashes are:

```text
neural   6c7f7e2e404a5c8ef8a9a3f632a24169b35d8be6a8c0ac496775bf9d32a07eb8
manual   39ac8deaf5382490a007bfd47ec7384989c64c6092eed70ac8ff682c076d8a57
ack-loss f15c88c6671db67934ce178e3f113b65ac2811a1538a0c36412f6c156bd279fd
```

Verify, decompress, and re-analyze without normalizing ANSI bytes or line
endings:

```sh
gzip -cd neural-qemu.log.gz | sha256sum
gzip -cd neural-qemu.log.gz > /tmp/ivc-neural-qemu.log
python3 ../../ivc/analyze_qemu.py /tmp/ivc-neural-qemu.log \
  --output /tmp/ivc-neural-summary.json --expected-count 1800

gzip -cd ack-loss-qemu.log.gz > /tmp/ivc-ack-loss-qemu.log
python3 ../../ivc/analyze_qemu.py /tmp/ivc-ack-loss-qemu.log \
  --output /tmp/ivc-ack-loss-summary.json --expected-count 100 \
  --profile ack-loss --drop-ack-every 5
```

The Linux rootfs was freshly built at SHA-256
`3dad2a5733e066b09def9dcbd063adaaf1407df0f344c0be6a4b566f1aa945d5`.
Each campaign used a private copy; mounting and ext4 recovery changed metadata
in those per-run copies, so neither 2 GiB mutable image nor its post-run hash is
retained as evidence. The static controller and both normal/fault Zephyr
image hashes are in `metadata.json` and can be regenerated from checked-in
sources.

QEMU TCG and WSL2 host scheduling contribute to observed latency. These runs
demonstrate the complete emulated data/control path and fault behavior, not a
physical-network or hardware latency bound.
