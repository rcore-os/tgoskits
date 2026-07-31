#!/usr/bin/env python3

import argparse
import socket
import statistics
import struct
import sys
import threading
import time


MAGIC = 0x51435A31  # QCZ1
VERSION = 1
HEADER_LEN = 28
CHECKSUM_OFFSET = 24

MSG_CONTROL_SET = 1
MSG_STATE_REQ = 2
MSG_ACK = 3
MSG_STATUS = 4
MSG_ERROR = 5

FLAG_DUPLICATE = 1

STATUS_OK = 0
STATUS_DUPLICATE = 1

HEADER = struct.Struct("!IBBHHHIQI")
CONTROL_PAYLOAD = struct.Struct("!iiI")
ACK_PAYLOAD = struct.Struct("!IIIi")
STATUS_PAYLOAD = struct.Struct("!IIiiiIII")


def checksum(frame: bytes) -> int:
    data = bytearray(frame)
    data[CHECKSUM_OFFSET : CHECKSUM_OFFSET + 4] = b"\x00\x00\x00\x00"
    value = 2166136261
    for byte in data:
        value ^= byte
        value = (value * 16777619) & 0xFFFFFFFF
    return value


def build_frame(msg_type: int, seq: int, payload: bytes = b"") -> bytes:
    timestamp_ns = time.monotonic_ns()
    header = HEADER.pack(
        MAGIC,
        VERSION,
        msg_type,
        HEADER_LEN,
        len(payload),
        0,
        seq,
        timestamp_ns,
        0,
    )
    frame = header + payload
    return frame[:CHECKSUM_OFFSET] + struct.pack("!I", checksum(frame)) + frame[CHECKSUM_OFFSET + 4 :]


def parse_frame(frame: bytes) -> dict:
    if len(frame) < HEADER_LEN:
        raise ValueError(f"short frame: {len(frame)}")

    magic, version, msg_type, header_len, payload_len, flags, seq, timestamp_ns, got = HEADER.unpack(
        frame[:HEADER_LEN]
    )
    if magic != MAGIC:
        raise ValueError(f"bad magic: 0x{magic:08x}")
    if version != VERSION:
        raise ValueError(f"bad version: {version}")
    if header_len != HEADER_LEN or len(frame) != header_len + payload_len:
        raise ValueError(
            f"bad length: header={header_len} payload={payload_len} actual={len(frame)}"
        )

    actual = checksum(frame)
    if got != actual:
        raise ValueError(f"bad checksum: got=0x{got:08x} actual=0x{actual:08x}")

    return {
        "type": msg_type,
        "flags": flags,
        "seq": seq,
        "timestamp_ns": timestamp_ns,
        "payload": frame[header_len:],
    }


def send_with_retry(sock: socket.socket, peer: tuple[str, int], seq: int, payload: bytes, args) -> tuple[bool, int, float, dict | None]:
    frame = build_frame(MSG_CONTROL_SET, seq, payload)
    attempts = 0
    started_ns = time.monotonic_ns()

    while attempts <= args.retries:
        attempts += 1
        sock.sendto(frame, peer)

        try:
            response, _ = sock.recvfrom(4096)
            parsed = parse_frame(response)
        except TimeoutError:
            continue
        except ValueError as exc:
            print(f"seq={seq} attempt={attempts} result=BAD_RESPONSE error={exc}")
            continue

        if parsed["type"] == MSG_ERROR:
            err_seq, status = struct.unpack("!II", parsed["payload"])
            print(f"seq={seq} attempt={attempts} result=ERROR err_seq={err_seq} status={status}")
            return False, attempts, (time.monotonic_ns() - started_ns) / 1_000_000, parsed

        if parsed["type"] != MSG_ACK or parsed["seq"] != seq:
            print(
                f"seq={seq} attempt={attempts} result=UNEXPECTED type={parsed['type']} ack_seq={parsed['seq']}"
            )
            continue

        ack_seq, status, applied_count, output_milli = ACK_PAYLOAD.unpack(parsed["payload"])
        latency_ms = (time.monotonic_ns() - started_ns) / 1_000_000
        duplicate = bool(parsed["flags"] & FLAG_DUPLICATE) or status == STATUS_DUPLICATE
        print(
            f"seq={seq} attempt={attempts} result=ACK ack_seq={ack_seq} "
            f"status={status} duplicate={int(duplicate)} applied_count={applied_count} "
            f"output_milli={output_milli} latency_ms={latency_ms:.3f}"
        )
        return status in (STATUS_OK, STATUS_DUPLICATE), attempts, latency_ms, parsed

    latency_ms = (time.monotonic_ns() - started_ns) / 1_000_000
    print(f"seq={seq} attempts={attempts} result=TIMEOUT latency_ms={latency_ms:.3f}")
    return False, attempts, latency_ms, None


def validate_status_frame(parsed: dict, expected_seq: int, expected_last_seq: int) -> bool:
    if parsed["type"] != MSG_STATUS:
        print(f"status result=UNEXPECTED type={parsed['type']}")
        print("QC_QCZ1_PY_STATUS_VALIDATION=BAD_FRAME")
        return False
    if len(parsed["payload"]) != STATUS_PAYLOAD.size:
        print(f"status result=BAD_PAYLOAD payload_len={len(parsed['payload'])}")
        print("QC_QCZ1_PY_STATUS_VALIDATION=BAD_FRAME")
        return False

    last_seq, status, setpoint, score, output, applied, duplicates, errors = STATUS_PAYLOAD.unpack(
        parsed["payload"]
    )
    print(
        f"status result=STATUS frame_seq={parsed['seq']} last_seq={last_seq} status={status} "
        f"setpoint_milli={setpoint} ai_score_milli={score} output_milli={output} "
        f"applied_count={applied} duplicate_count={duplicates} error_count={errors}"
    )
    if parsed["seq"] != expected_seq:
        print(
            f"QC_QCZ1_PY_STATUS_VALIDATION=SEQ_MISMATCH "
            f"expected_frame_seq={expected_seq} got_frame_seq={parsed['seq']}"
        )
        return False
    if last_seq != expected_last_seq:
        print(
            f"QC_QCZ1_PY_STATUS_VALIDATION=LAST_SEQ_MISMATCH "
            f"expected_last_seq={expected_last_seq} got_last_seq={last_seq}"
        )
        return False
    if status != STATUS_OK:
        print(f"QC_QCZ1_PY_STATUS_VALIDATION=STATUS_UNHEALTHY status={status}")
        return False
    if errors != 0:
        print(f"QC_QCZ1_PY_STATUS_VALIDATION=ERROR_COUNT_NONZERO error_count={errors}")
        return False

    print("QC_QCZ1_PY_STATUS_VALIDATION=OK")
    return True


def request_status(
    sock: socket.socket,
    peer: tuple[str, int],
    seq: int,
    timeout: float,
    expected_last_seq: int,
) -> bool:
    sock.settimeout(timeout)
    sock.sendto(build_frame(MSG_STATE_REQ, seq), peer)
    try:
        response, _ = sock.recvfrom(4096)
        parsed = parse_frame(response)
    except TimeoutError:
        print("status result=TIMEOUT")
        print("QC_QCZ1_PY_STATUS_VALIDATION=IO_ERROR")
        return False
    except OSError as exc:
        print(f"status result=IO_ERROR error={exc}")
        print("QC_QCZ1_PY_STATUS_VALIDATION=IO_ERROR")
        return False
    except ValueError as exc:
        print(f"status result=BAD_FRAME error={exc}")
        print("QC_QCZ1_PY_STATUS_VALIDATION=BAD_FRAME")
        return False

    return validate_status_frame(parsed, seq, expected_last_seq)


def build_status_payload(
    last_seq: int,
    status: int = STATUS_OK,
    setpoint: int = 1000,
    score: int = 800,
    output: int = 800,
    applied: int = 10,
    duplicates: int = 0,
    errors: int = 0,
) -> bytes:
    return STATUS_PAYLOAD.pack(last_seq, status, setpoint, score, output, applied, duplicates, errors)


def run_status_validation_selftest() -> int:
    expected_seq = 122
    expected_last_seq = 10

    def require_result(name: str, ok: bool, expected_ok: bool) -> None:
        if ok != expected_ok:
            print(
                f"QC_QCZ1_PY_STATUS_NEGATIVE_SELFTEST_FAIL case={name} "
                f"expected_ok={int(expected_ok)} got_ok={int(ok)}"
            )
            raise SystemExit(1)

    def require_case(name: str, frame: bytes, expected_ok: bool) -> None:
        parsed = parse_frame(frame)
        require_result(name, validate_status_frame(parsed, expected_seq, expected_last_seq), expected_ok)

    def require_udp_case(name: str, response_frame: bytes, expected_ok: bool) -> None:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as server:
            server.bind(("127.0.0.1", 0))
            peer = server.getsockname()

            def serve_once() -> None:
                request, client_addr = server.recvfrom(4096)
                parsed_request = parse_frame(request)
                if parsed_request["type"] != MSG_STATE_REQ or parsed_request["seq"] != expected_seq:
                    return
                server.sendto(response_frame, client_addr)

            thread = threading.Thread(target=serve_once, daemon=True)
            thread.start()
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as client:
                ok = request_status(client, peer, expected_seq, 0.5, expected_last_seq)
            thread.join(timeout=1.0)
            require_result(name, ok, expected_ok)

    require_case("ok", build_frame(MSG_STATUS, expected_seq, build_status_payload(expected_last_seq)), True)
    require_case(
        "stale-frame-seq",
        build_frame(MSG_STATUS, expected_seq - 1, build_status_payload(expected_last_seq)),
        False,
    )
    require_case(
        "stale-last-seq",
        build_frame(MSG_STATUS, expected_seq, build_status_payload(0)),
        False,
    )
    require_case(
        "unhealthy-status",
        build_frame(MSG_STATUS, expected_seq, build_status_payload(expected_last_seq, status=99)),
        False,
    )
    require_case(
        "error-count",
        build_frame(MSG_STATUS, expected_seq, build_status_payload(expected_last_seq, errors=7)),
        False,
    )
    require_case(
        "wrong-type",
        build_frame(MSG_ACK, expected_seq, ACK_PAYLOAD.pack(expected_seq, STATUS_OK, 1, 800)),
        False,
    )
    require_case("bad-payload", build_frame(MSG_STATUS, expected_seq, b""), False)
    require_udp_case(
        "udp-ok",
        build_frame(MSG_STATUS, expected_seq, build_status_payload(expected_last_seq)),
        True,
    )
    require_udp_case(
        "udp-poisoned-status",
        build_frame(MSG_STATUS, expected_seq, build_status_payload(0, status=99, errors=7)),
        False,
    )
    print("QC_QCZ1_PY_STATUS_NEGATIVE_SELFTEST=PASS")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Reliable UDP control client for the Quancheng AxVisor RTOS guest demo.")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=14242)
    parser.add_argument("--count", type=int, default=20)
    parser.add_argument("--timeout", type=float, default=0.5)
    parser.add_argument("--interval", type=float, default=0.05)
    parser.add_argument("--retries", type=int, default=3)
    parser.add_argument("--duplicate-every", type=int, default=0)
    parser.add_argument("--setpoint-base", type=int, default=1000)
    parser.add_argument("--ai-score-base", type=int, default=800)
    parser.add_argument(
        "--selftest-status-validation",
        action="store_true",
        help="run local QCZ1 STATUS validation negative selftest and exit",
    )
    args = parser.parse_args()

    if args.selftest_status_validation:
        return run_status_validation_selftest()

    peer = (args.host, args.port)
    successes = 0
    failures = 0
    retransmits = 0
    latencies_ms = []

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.settimeout(args.timeout)

        for seq in range(1, args.count + 1):
            setpoint = args.setpoint_base + seq * 10
            score = args.ai_score_base + (seq % 5) * 25
            payload = CONTROL_PAYLOAD.pack(setpoint, score, seq)
            ok, attempts, latency_ms, _ = send_with_retry(sock, peer, seq, payload, args)
            retransmits += max(0, attempts - 1)
            if ok:
                successes += 1
                latencies_ms.append(latency_ms)
            else:
                failures += 1

            if args.duplicate_every and seq % args.duplicate_every == 0:
                ok, attempts, latency_ms, _ = send_with_retry(sock, peer, seq, payload, args)
                retransmits += max(0, attempts - 1)
                if ok:
                    latencies_ms.append(latency_ms)
                else:
                    failures += 1

            time.sleep(args.interval)

        status_ok = request_status(
            sock,
            peer,
            args.count + 1000,
            args.timeout,
            expected_last_seq=args.count,
        )

    print(
        f"summary requested={args.count} successes={successes} failures={failures} "
        f"retransmits={retransmits} status_ok={int(status_ok)}"
    )
    if latencies_ms:
        ordered = sorted(latencies_ms)
        p95_index = max(0, int(len(ordered) * 0.95 + 0.999999) - 1)
        print(
            f"latency_ms min={min(ordered):.3f} mean={statistics.fmean(ordered):.3f} "
            f"p95={ordered[p95_index]:.3f} max={max(ordered):.3f}"
        )

    return 0 if failures == 0 and successes == args.count and status_ok else 1


if __name__ == "__main__":
    sys.exit(main())
