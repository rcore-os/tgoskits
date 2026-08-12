#!/usr/bin/env python3
"""Host-side T2N1 responder used by the single-Guest acceptance run.

The responder is deliberately independent of the Guest endpoint.  It receives
the controller's UDP datagrams, validates the wire frame and sends protocol
responses to the source address returned by ``recvfrom``.  This prevents the
P1 experiment from accidentally feeding a controller's own datagrams back into
its socket.
"""

from __future__ import annotations

import argparse
import socket
import struct
import sys
import time
import zlib
from dataclasses import dataclass


FRAME_MAGIC = b"T2N1"
PROTOCOL_VERSION = 1
FRAME_HEADER_LEN = 28
MAX_PAYLOAD_LEN = 1200
RELIABLE_FLAG = 1

CONTROL_KIND = 1
STATUS_KIND = 2
ERROR_KIND = 3
ACK_KIND = 4
HEARTBEAT_KIND = 5

INVALID_PARAMETER = 1
OUT_OF_ORDER = 2
SESSION_MISMATCH = 4

SESSION_ID = 0x5452_5432
CONTROL_PAYLOAD_LEN = 12
STATUS_PAYLOAD_LEN = 12


@dataclass(frozen=True)
class ParsedFrame:
    kind: int
    session_id: int
    sequence: int
    acknowledgement: int
    error_code: int
    reliable: bool
    payload: bytes


def _crc32(datagram: bytes) -> int:
    checksum_input = datagram[:24] + b"\x00\x00\x00\x00" + datagram[28:]
    return zlib.crc32(checksum_input) & 0xFFFFFFFF


def encode_frame(
    kind: int,
    session_id: int,
    sequence: int,
    acknowledgement: int,
    payload: bytes,
    *,
    reliable: bool = False,
    error_code: int = 0,
) -> bytes:
    """Encode one T2N1 frame using the protocol crate's wire layout."""

    if len(payload) > MAX_PAYLOAD_LEN:
        raise ValueError("payload exceeds protocol maximum")
    header = struct.pack(
        "!4sBBHIIIHHI",
        FRAME_MAGIC,
        PROTOCOL_VERSION,
        kind,
        RELIABLE_FLAG if reliable else 0,
        session_id,
        sequence,
        acknowledgement,
        len(payload),
        error_code,
        0,
    )
    datagram = header + payload
    checksum = _crc32(datagram)
    return datagram[:24] + struct.pack("!I", checksum) + datagram[28:]


def parse_frame(datagram: bytes) -> ParsedFrame:
    """Validate and decode one complete T2N1 datagram."""

    if len(datagram) < FRAME_HEADER_LEN:
        raise ValueError("frame header is truncated")
    magic, version, kind, flags, session_id, sequence, acknowledgement, payload_len, error_code, checksum = struct.unpack(
        "!4sBBHIIIHHI", datagram[:FRAME_HEADER_LEN]
    )
    if magic != FRAME_MAGIC or version != PROTOCOL_VERSION:
        raise ValueError("unsupported frame identity")
    if flags & ~RELIABLE_FLAG:
        raise ValueError("unknown frame flags")
    if payload_len > MAX_PAYLOAD_LEN or payload_len != len(datagram) - FRAME_HEADER_LEN:
        raise ValueError("invalid payload length")
    if checksum != _crc32(datagram):
        raise ValueError("frame checksum mismatch")
    payload = datagram[FRAME_HEADER_LEN:]
    reliable = bool(flags & RELIABLE_FLAG)
    if kind in (CONTROL_KIND, STATUS_KIND):
        if not reliable or sequence == 0 or acknowledgement != 0 or error_code != 0:
            raise ValueError("invalid reliable frame fields")
    elif kind == ACK_KIND:
        if reliable or sequence != 0 or acknowledgement == 0 or error_code != 0 or payload:
            raise ValueError("invalid ACK fields")
    elif kind == ERROR_KIND:
        if reliable or sequence != 0 or error_code == 0:
            raise ValueError("invalid ERROR fields")
    elif kind == HEARTBEAT_KIND:
        if reliable or sequence != 0 or acknowledgement != 0 or error_code != 0:
            raise ValueError("invalid heartbeat fields")
    else:
        raise ValueError("unknown message kind")
    return ParsedFrame(
        kind,
        session_id,
        sequence,
        acknowledgement,
        error_code,
        reliable,
        payload,
    )


def _ack(session_id: int, sequence: int) -> bytes:
    return encode_frame(ACK_KIND, session_id, 0, sequence, b"")


def _error(session_id: int, sequence: int, error_code: int) -> bytes:
    return encode_frame(ERROR_KIND, session_id, 0, sequence, b"", error_code=error_code)


def _status(session_id: int, sequence: int, state: int, value: int, request_id: int) -> bytes:
    payload = bytearray(STATUS_PAYLOAD_LEN)
    payload[0] = state
    payload[4:8] = value.to_bytes(4, "big", signed=True)
    payload[8:12] = request_id.to_bytes(4, "big")
    return encode_frame(STATUS_KIND, session_id, sequence, 0, bytes(payload), reliable=True)


class Task2Responder:
    """Minimal managed-side responder with duplicate and ordering checks."""

    def __init__(self, session_id: int = SESSION_ID) -> None:
        self.session_id = session_id
        self.expected_rx_sequence = 1
        self.next_tx_sequence = 1
        self.last_request_id = 0
        self.safe = False

    def heartbeat(self, uptime_ms: int) -> bytes:
        """Encode a liveness frame carrying this responder's monotonic uptime."""

        payload = uptime_ms.to_bytes(8, "big", signed=False)
        return encode_frame(HEARTBEAT_KIND, self.session_id, 0, 0, payload)

    def handle(self, datagram: bytes) -> list[bytes]:
        """Return protocol responses for one received datagram."""

        try:
            frame = parse_frame(datagram)
        except ValueError:
            return []
        if frame.session_id != self.session_id:
            return [_error(self.session_id, frame.sequence, SESSION_MISMATCH)]

        if frame.kind == HEARTBEAT_KIND:
            self.safe = False
            return []
        if frame.kind == ACK_KIND:
            return []
        if frame.kind == ERROR_KIND:
            self.safe = True
            return []
        if frame.kind == STATUS_KIND:
            return self._receive_reliable(frame)
        return self._receive_control(frame)

    def _receive_reliable(self, frame: ParsedFrame) -> list[bytes]:
        if frame.sequence == self.expected_rx_sequence:
            self.expected_rx_sequence = _next_sequence(self.expected_rx_sequence)
            return [_ack(self.session_id, frame.sequence)]
        if frame.sequence == _previous_sequence(self.expected_rx_sequence):
            return [_ack(self.session_id, frame.sequence)]
        self.safe = True
        return [_error(self.session_id, frame.sequence, OUT_OF_ORDER)]

    def _receive_control(self, frame: ParsedFrame) -> list[bytes]:
        if len(frame.payload) != CONTROL_PAYLOAD_LEN:
            self.safe = True
            return [_error(self.session_id, frame.sequence, INVALID_PARAMETER)]
        if frame.payload[1:4] != b"\x00\x00\x00":
            self.safe = True
            return [_error(self.session_id, frame.sequence, INVALID_PARAMETER)]
        action = frame.payload[0]
        value = int.from_bytes(frame.payload[4:8], "big", signed=True)
        request_id = int.from_bytes(frame.payload[8:12], "big")
        if action == 1 and not 0 <= value <= 1000:
            self.safe = True
            return [_error(self.session_id, frame.sequence, INVALID_PARAMETER)]
        if action in (2, 3) and value != 0:
            self.safe = True
            return [_error(self.session_id, frame.sequence, INVALID_PARAMETER)]
        if frame.sequence == self.expected_rx_sequence:
            self.expected_rx_sequence = _next_sequence(self.expected_rx_sequence)
            self.last_request_id = request_id
            state = 2 if action == 2 else 1
            self.safe = action != 3 and self.safe
            status = _status(
                self.session_id,
                self.next_tx_sequence,
                state,
                value,
                request_id,
            )
            self.next_tx_sequence = _next_sequence(self.next_tx_sequence)
            return [_ack(self.session_id, frame.sequence), status]
        if frame.sequence == _previous_sequence(self.expected_rx_sequence):
            return [_ack(self.session_id, frame.sequence)]
        self.safe = True
        return [_error(self.session_id, frame.sequence, OUT_OF_ORDER)]


def _next_sequence(sequence: int) -> int:
    value = (sequence + 1) & 0xFFFFFFFF
    return 1 if value == 0 else value


def _previous_sequence(sequence: int) -> int:
    return 0xFFFFFFFF if sequence <= 1 else sequence - 1


def run(bind: str, port: int, session_id: int) -> int:
    responder = Task2Responder(session_id)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind((bind, port))
        print(f"TASK2_RESPONDER_READY bind={bind}:{port} session=0x{session_id:08x}", flush=True)
        started = time.monotonic()
        next_heartbeat = started + 0.2
        peer = None
        while True:
            now = time.monotonic()
            sock.settimeout(max(0.01, min(0.05, next_heartbeat - now)))
            try:
                datagram, peer = sock.recvfrom(FRAME_HEADER_LEN + MAX_PAYLOAD_LEN)
            except socket.timeout:
                pass
            except KeyboardInterrupt:
                print("TASK2_RESPONDER_STOP", flush=True)
                return 0
            else:
                responses = responder.handle(datagram)
                try:
                    frame = parse_frame(datagram)
                    print(
                        f"TASK2_RESPONDER_RX kind={frame.kind} seq={frame.sequence} peer={peer[0]}:{peer[1]}",
                        flush=True,
                    )
                except ValueError:
                    print("TASK2_RESPONDER_RX_INVALID", flush=True)
                for response in responses:
                    sock.sendto(response, peer)
                    response_frame = parse_frame(response)
                    print(
                        f"TASK2_RESPONDER_TX kind={response_frame.kind} seq={response_frame.sequence} "
                        f"ack={response_frame.acknowledgement}",
                        flush=True,
                    )

            now = time.monotonic()
            if peer is not None and now >= next_heartbeat:
                heartbeat = responder.heartbeat(int((now - started) * 1000))
                sock.sendto(heartbeat, peer)
                print("TASK2_RESPONDER_TX kind=5 seq=0 ack=0", flush=True)
                next_heartbeat = now + 0.2


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bind", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=4242)
    parser.add_argument("--session-id", type=lambda value: int(value, 0), default=SESSION_ID)
    args = parser.parse_args()
    try:
        return run(args.bind, args.port, args.session_id)
    except OSError as error:
        print(f"TASK2_RESPONDER_ERROR={error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
