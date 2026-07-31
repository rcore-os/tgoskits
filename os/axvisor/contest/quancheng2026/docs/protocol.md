# Reliable UDP Control Protocol

The contest transport is UDP over IPv4. Reliability is implemented at the application layer by sequence numbers, ACKs, timeouts, retries and duplicate detection.

Plain UDP packets that do not begin with the `QCZ1` magic are echoed unchanged. This keeps the original Zephyr echo-server sanity check available while adding the contest protocol on the same port.

## Header

All integer fields are big-endian.

```text
offset  size  field
0       4     magic = 0x51435a31 ("QCZ1")
4       1     version = 1
5       1     message_type
6       2     header_len = 28
8       2     payload_len
10      2     flags
12      4     sequence
16      8     timestamp_ns
24      4     checksum
28      N     payload
```

The checksum is FNV-1a over the full frame with the checksum field zeroed.

## Message Types

```text
1 CONTROL_SET
2 STATE_REQ
3 ACK
4 STATUS
5 ERROR
```

## CONTROL_SET Payload

```text
offset  size  field
0       4     setpoint_milli, signed
4       4     ai_score_milli, signed
8       4     client_sample_id
```

The RTOS side computes an observable demo output:

```text
output_milli = setpoint_milli * ai_score_milli / 1000
```

This is logged on the RTOS serial console as `QC CTRL ... output_milli=...`.

## ACK Payload

```text
offset  size  field
0       4     acknowledged sequence
4       4     status
8       4     applied_count
12      4     output_milli, signed
```

If the RTOS receives a sequence number that has already been applied, it sends an ACK with:

```text
status = 1
flags bit 0 = duplicate
```

The duplicated command is not applied again.

## STATUS Payload

```text
offset  size  field
0       4     last_seq
4       4     status
8       4     setpoint_milli, signed
12      4     ai_score_milli, signed
16      4     output_milli, signed
20      4     applied_count
24      4     duplicate_count
28      4     error_count
```

## ERROR Payload

```text
offset  size  field
0       4     related sequence, or 0 if unavailable
4       4     error code
```

Current error codes:

```text
100 BAD_LENGTH
101 BAD_VERSION
102 BAD_CHECKSUM
103 UNSUPPORTED_TYPE
```

## Linux-Side Reliability

`linux/qc_reliable_udp_client.py` implements:

- response timeout
- bounded retransmission
- ACK sequence validation
- bad response reporting
- duplicate packet test support
- final status query with response frame sequence, `last_seq`, `status == OK`, and `error_count == 0` validation
- latency summary

The current 10-round campaign validated `200/200` reliable control messages and `40` duplicate ACK responses.

## Communication Metrics

The integrated dual-guest analyzer records the metrics required for the contest communication task:

- request success and failure counts for plain UDP echo, QCZ1 reliable control and AI control;
- application-level error indicators, including QCZ1 failure count, final RTOS `STATUS` error count and duplicate ACK handling;
- timeout/recovery indicators, including UDP retry count, QCZ1 retransmission count and duplicate command suppression;
- request/response latency distributions with min, mean, p50, p95, p99 and max values;
- tcpdump packet counts and kernel-drop counts on the bridge;
- effective application throughput, reported as a conservative serialized request/response estimate: successful transactions divided by the sum of observed request latencies for that channel.

This throughput number is intentionally application-level. It measures the useful command/response rate of the contest protocol path, while raw link capacity remains outside the scope of the small deterministic control workload.
