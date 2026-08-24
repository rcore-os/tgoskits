# ATK-DLRK3588 Task 2/3 FIFO A/B evidence (2026-08-23)

> Repository snapshot note: generated raw/FIT images and the DTB are omitted
> from Git. Their exact SHA256 identities are retained in the parent
> `artifact-hashes.txt`; all reports, configurations, pcaps, and formal logs
> needed to audit the claims remain here.

Status: **complete physical-board addendum for Task 2 and Task 3**. The FIFO
control loop, short dual-ended pcap, host statistics, and a live
blackout-to-Safe-to-recovery run are archived and verified. The earlier
2600-frame dump failure is retained below as a separate AxVisor diagnostic.

## Scope and configuration

- Source tree: `/home/huhu/tgoskits-starry`
- Source commit: `afec9c6046f357ee8b6762829633859cf419255f`
- The source tree had pre-existing tracked and untracked changes and was not
  cleaned or reset.
- Guest configs and payloads were held constant between the RR and FIFO runs.
- The only board-config A/B variable was `rr-scheduler`. The archived
  `atk-task23-board.toml` omits it and therefore selects the default FIFO
  scheduler.
- The Claude session shows that the original Task 2/3 board config was copied
  from the Task 1 RR config with the no-op command
  `sed 's/"rr-scheduler",/"rr-scheduler",/'`.

Build command corresponding to the archived FIFO configuration:

```sh
cargo xtask axvisor build \
  --config /home/huhu/atk-task23-board.toml \
  --vmconfigs /home/huhu/atk-task23-starry.toml \
  --vmconfigs /home/huhu/atk-task23-rtthread.toml
```

The ELF was converted with `aarch64-linux-gnu-objcopy -O binary` and wrapped by
`mkimage -f axvisor-task23-fifo.its axvisor-task23-fifo.fit`.

## RAM-only boot

No eMMC write command was used. U-Boot entered `fastboot usb 0`, and the host
sent:

```sh
fastboot stage /home/huhu/atk-bringup/axvisor-task23-fifo.fit
```

U-Boot then extracted the FIT components in RAM and ran:

```text
fdt addr 0x00c00800
fdt get addr fdtsrc /images/fdt-1 data
fdt get size fdtlen /images/fdt-1 data
fdt get value fdtdst /images/fdt-1 load
fdt get addr kernelsrc /images/kernel-1 data
fdt get size kernellen /images/kernel-1 data
fdt get value kerneldst /images/kernel-1 load
cp.b ${fdtsrc} ${fdtdst} ${fdtlen}
cp.b ${kernelsrc} ${kerneldst} ${kernellen}
fdt addr ${fdtdst}
booti ${kerneldst} - ${fdtdst}
```

## Results

RR run (`task23-live-console-20260823.log`):

- Three completed control loops, then failure while request 4 was in progress.
- Failure markers: `ESR_EL2=0x96000004`, `ESR_EL2=0x86000004`, and repeated
  `Unhandled acknowledged host IRQ 26`.
- Task 3 elapsed time before failure: approximately 24 seconds.

FIFO run (`task23-fifo-console-20260823.log`), before capture dump:

- `TASK3_INFER`: 137
- `TASK3_CONTROL_SENT`: 137
- `TASK2_ACK`: 137
- `TASK3_STATUS_RECEIVED`: 137
- Closed-loop elapsed time: 329.188 seconds
- RTT: min 45 ms, p50 243 ms, p90 250 ms, p95 252 ms, p99 261 ms,
  max 278 ms, mean 239.78 ms
- `ESR_EL2`: 0
- `Unhandled acknowledged host IRQ 26`: 0
- Request 138 had started when the console was safely detached with `Ctrl-X,h`.

The follow-up short capture was dumped only after both VMs were suspended. It
ended normally with `CAPDUMP_END`, yielded 83 packets per port, and passed the
dual-pcap verifier at a 95% minimum ACK rate:

```text
VM1: packets=83 udp=75 task2=75 kinds={5:8, 1:17, 4:33, 2:17}
VM2: packets=83 udp=75 task2=75 kinds={5:8, 1:17, 4:33, 2:17}
PASS
```

The same live Guests then passed an uncaptured blackout test. VM1 retransmitted
CONTROL sequence 164 five times and entered Safe through `RetryExhausted`; VM2
entered Safe through `HeartbeatTimeout`. After `virtnet drop off`, VM1 reported
`TASK2_RECOVERED state=Active`, restarted its reliable sequence at 1, and the
two Guests resumed CONTROL/ACK/STATUS exchanges. The final switch state was
blackout off and both VMs remained `running`.

Guest initrd validation:

- Size: 27,400,910 bytes
- SHA256: `e61b388e1abbe872ab401ec305329ae144c2c5660bcc664a180c21ab9f67e5b8`
- `gzip -t`: passed

## Separate capture-dump defect

Capture was left enabled after detaching, so the buffer reached 2600 frames.
After `virtnet capture off`, `virtnet capture dump` emitted `CAPDUMP_BEGIN` and
37 complete VM1 records. During the next record, three EL2 data-abort messages
interleaved with the hex stream and the host stopped responding. There was no
`CAPDUMP_END` and no VM2 section, so this truncated stream must not be cited as
a valid dual-ended pcap.

The capture records carry timestamps around 560--574 seconds, while the
exception timestamp is 661.745 seconds. This shows that the exception occurred
during dumping of old buffered data, not as a spontaneous FIFO control-loop
failure. The successful short-capture follow-up above avoids this path by
stopping at 2278 buffered frames, suspending both Guests, and dumping while
their producers are quiescent.

## Task-specific reports

- `TASK2.md`: network, protocol, reliability, recovery, isolation, pcap, and
  automated-test evidence.
- `TASK3.md`: real in-Guest ncnn/YOLO assets, policy, detection, control-loop,
  RTT, and recovery evidence.

## Integrity

In the full external archive, run `sha256sum -c MANIFEST.sha256` from that
archive root; it covers all inputs, images, reports, pcaps, and verification
logs except the manifest itself. In the compact repository snapshot, run
`sha256sum -c SHA256SUMS.txt` from the snapshot root instead.
