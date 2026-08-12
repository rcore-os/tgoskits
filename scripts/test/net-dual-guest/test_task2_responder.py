#!/usr/bin/env python3
"""Deterministic tests for the host-side T2N1 responder."""

from __future__ import annotations

from task2_responder import (
    CONTROL_KIND,
    HEARTBEAT_KIND,
    STATUS_KIND,
    ACK_KIND,
    SESSION_ID,
    Task2Responder,
    encode_frame,
    parse_frame,
)


def control_frame(sequence: int = 1, request_id: int = 7) -> bytes:
    payload = bytearray(12)
    payload[0] = 1  # SetOutput
    payload[4:8] = (100).to_bytes(4, "big", signed=True)
    payload[8:12] = request_id.to_bytes(4, "big")
    return encode_frame(CONTROL_KIND, SESSION_ID, sequence, 0, bytes(payload), reliable=True)


def test_control_produces_ack_and_status_to_the_same_peer() -> None:
    responder = Task2Responder()

    responses = responder.handle(control_frame())

    assert len(responses) == 2
    assert int.from_bytes(responses[0][5:6], "big") == ACK_KIND
    assert int.from_bytes(responses[0][16:20], "big") == 1
    assert int.from_bytes(responses[1][5:6], "big") == STATUS_KIND
    assert int.from_bytes(responses[1][12:16], "big") == 1


def test_duplicate_control_is_acked_without_second_status() -> None:
    responder = Task2Responder()
    responder.handle(control_frame())

    responses = responder.handle(control_frame())

    assert len(responses) == 1
    assert int.from_bytes(responses[0][5:6], "big") == ACK_KIND
    assert int.from_bytes(responses[0][16:20], "big") == 1


def test_heartbeat_contains_sender_uptime() -> None:
    responder = Task2Responder()

    frame = parse_frame(responder.heartbeat(1234))

    assert frame.kind == HEARTBEAT_KIND
    assert int.from_bytes(frame.payload, "big") == 1234
