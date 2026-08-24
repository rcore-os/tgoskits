#!/usr/bin/env python3
"""Deterministic tests for the host-side T2N1 responder."""

from __future__ import annotations

from pathlib import Path

from task2_responder import (
    CONTROL_KIND,
    ERROR_KIND,
    HEARTBEAT_KIND,
    STATUS_KIND,
    ACK_KIND,
    SESSION_ID,
    SESSION_MISMATCH,
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


def test_session_mismatch_heartbeat_returns_correlated_error() -> None:
    responder = Task2Responder()
    foreign_session = SESSION_ID ^ 0x00000001
    heartbeat = encode_frame(
        HEARTBEAT_KIND,
        foreign_session,
        0,
        0,
        (1234).to_bytes(8, "big"),
    )

    responses = responder.handle(heartbeat)

    assert len(responses) == 1
    error = parse_frame(responses[0])
    assert error.kind == ERROR_KIND
    assert error.session_id == SESSION_ID
    assert error.sequence == 0
    assert error.acknowledgement == 0
    assert error.error_code == SESSION_MISMATCH
    assert responder.safe is False


def test_session_mismatch_reliable_frame_correlates_sequence() -> None:
    responder = Task2Responder()
    foreign_session = SESSION_ID ^ 0x00000001
    frame = control_frame(sequence=7)
    frame = encode_frame(
        CONTROL_KIND,
        foreign_session,
        7,
        0,
        frame[28:],
        reliable=True,
    )

    responses = responder.handle(frame)

    assert len(responses) == 1
    error = parse_frame(responses[0])
    assert error.kind == ERROR_KIND
    assert error.error_code == SESSION_MISMATCH
    assert error.acknowledgement == 7
    assert responder.safe is False


def test_zephyr_endpoint_keeps_session_mismatch_out_of_malformed_path() -> None:
    source = (Path(__file__).parent / "zephyr-task2/src/main.c").read_text()

    assert "uint32_t *session_id" in source
    assert "else if (session_id != SESSION_ID)" in source
    assert "TASK2_SESSION_MISMATCH" in source
    assert "ERROR_SESSION_MISMATCH" in source


def test_both_rtos_endpoints_keep_protocol_safety_contracts_aligned() -> None:
    root = Path(__file__).parent
    sources = [
        (root / "rtthread-task2/main.c").read_text(),
        (root / "zephyr-task2/src/main.c").read_text(),
    ]

    for source in sources:
        assert "else if (!same_peer(&source, &peer))" in source
        assert source.index("else if (!same_peer(&source, &peer))") < source.index("last_rx = now")
        assert "else if (kind == KIND_ERROR)" in source
        assert "event=RemoteError" in source
        assert "event=SendFailure" in source
        assert "status_payload[0] = managed_state" in source
        assert "if (action == 1)" in source
        assert "else if (action == 2)" in source
        assert "*error > ERROR_SESSION_MISMATCH" in source
        assert "TASK2_REJECTED invalid_ack" in source
