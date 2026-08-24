# Task 2 physical-board completion report

> This is the compact repository snapshot. Generated images are identified by
> the parent `artifact-hashes.txt` and remain in the authenticated full archive.

Status: **PASS** for the ATK-DLRK3588 physical-board addendum. The repository's
canonical seven-scenario QEMU evidence remains the exhaustive protocol matrix;
this addendum independently demonstrates the real-board data plane, wire
ledger, retry exhaustion, Safe entry, and recovery.

## Topology and isolation

```text
StarryOS VM1  10.0.42.15:4242  MAC 52:54:00:12:34:15
        |  independent stage-2 memory + VirtIO-net port
        |  AxVisor internal L2 switch / blackout gate
        |  independent stage-2 memory + VirtIO-net port
RT-Thread VM2 10.0.42.2:4242   MAC 52:54:00:12:34:02
```

The archived VM configs allocate separate 1 GiB and 512 MiB regions, bind VM1
to pCPU `0x00` and VM2 to pCPU `0x100`, define distinct MAC addresses, and use
separate virtual devices/vIRQ routes. Both Guests remained running after the
fault test.

## Normal dual-ended capture

`task23-fifo-short.vm1.pcap` and `task23-fifo-short.vm2.pcap` contain identical
T2N1 ledgers at the two switch boundaries:

| Per pcap | Count |
|---|---:|
| packets | 83 |
| UDP / T2N1 | 75 / 75 |
| HEARTBEAT | 8 |
| CONTROL | 17 |
| ACK | 33 |
| STATUS | 17 |

The verifier passed with `--require-task2 --min-ack-rate 95`; 33 of the 34
reliable frames visible in the deliberately short window were acknowledged
(97.1%). `task2-pcap-verify.log` is the retained output.

## Reliability and recovery

The live hypervisor blackout gate was enabled while both protocol stacks and
the ncnn loop kept running. The ordered evidence in
`task23-fifo-normal-blackout-20260823.log` is:

1. `virtnet: blackout ON`.
2. VM1 retransmitted CONTROL `seq=164`, attempts 1 through 5.
3. VM1 entered `Safe` through `RetryExhausted`.
4. VM2 entered `Safe` through `HeartbeatTimeout`.
5. `virtnet: blackout OFF`.
6. VM1 reported `TASK2_RECOVERED state=Active`.
7. The reliable stream restarted at sequence 1 and completed ACK + STATUS;
   after recovery the retained consoles contain 19 RT-Thread CONTROL/STATUS
   exchanges and 12 StarryOS CONTROL/STATUS completions.
8. The final switch state was blackout off; VM1 and VM2 were both `running`.

`task2-blackout-verify.log` mechanically checks this ordering and final state.

## Automated checks

- `task2-net-protocol`: 21/21 passed, including CRC/framing, duplicate
  suppression, retry exhaustion, out-of-order, invalid payload,
  session-mismatch, Safe, and resynchronization tests.
- Task 2/3 Python network and evidence modules: 22/22 passed.
- The archived `task23-python-tests.log` is an earlier diagnostic in which two
  Task 1 A/B verifier fixtures lacked the newly required `rtos_name` argument.
  That verifier defect is now fixed: the final rerun is 6/6 for Task 1 and
  28/28 for the full directory, retained in the separate Task 1 physical-board
  archive. The historical log remains here for provenance and does not affect
  the Task 2/3 targeted PASS.

## Reproduction

The board was booted RAM-only with `fastboot stage`; no eMMC write or
`fastboot boot` was used. The exact FIT, DTB, AxVisor/VM configs, raw logs,
pcaps, verifier outputs, and hashes are retained in this directory.
