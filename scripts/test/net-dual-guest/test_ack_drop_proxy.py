#!/usr/bin/env python3
"""Deterministic tests for the QEMU frame parser and drop policy."""

from __future__ import annotations

from collections import Counter
from io import StringIO

from ack_drop_proxy import AckDropProxy, Task2Metadata, task2_metadata
from verify_fault_pcap import check_expected_fault_delta


def ethernet_udp_frame(payload: bytes) -> bytes:
    ethernet = bytes.fromhex("5254001234015254001234020800")
    ip_header = bytearray(20)
    ip_header[0] = 0x45
    ip_header[9] = 17
    udp_header = bytes(8)
    return ethernet + bytes(ip_header) + udp_header + payload


def test_extracts_task2_ack_metadata() -> None:
    payload = bytearray(28)
    payload[:4] = b"T2N1"
    payload[5] = 4
    payload[12:16] = (17).to_bytes(4, "big")

    assert task2_metadata(ethernet_udp_frame(bytes(payload))) == Task2Metadata(4, 17, 0)


def test_non_task2_frames_are_not_classified() -> None:
    assert task2_metadata(ethernet_udp_frame(b"ordinary-udp")) is None
    assert task2_metadata(bytes(60)) is None


def test_drop_policy_is_bounded_to_direction_and_count() -> None:
    proxy = AckDropProxy(12731, 12732, "rtos-to-linux", 4, 1, "none", StringIO())
    metadata = Task2Metadata(kind=4, sequence=9, acknowledgement=9)

    assert proxy._should_drop(metadata, "linux-to-rtos") is False
    assert proxy._should_drop(metadata, "rtos-to-linux") is True
    assert proxy._should_drop(metadata, "rtos-to-linux") is False


def test_blackout_window_is_active_between_start_and_end() -> None:
    proxy = AckDropProxy(
        12731,
        12732,
        "rtos-to-linux",
        4,
        1,
        "none",
        StringIO(),
        blackout_start_ms=1000,
        blackout_duration_ms=500,
    )

    assert proxy.blackout_active_at(0) is False
    assert proxy.blackout_active_at(999) is False
    assert proxy.blackout_active_at(1000) is True
    assert proxy.blackout_active_at(1499) is True
    assert proxy.blackout_active_at(1500) is False
    assert proxy.blackout_active_at(10000) is False


def test_blackout_is_disabled_without_a_start() -> None:
    proxy = AckDropProxy(12731, 12732, "rtos-to-linux", 4, 1, "none", StringIO())

    assert proxy.blackout_active_at(0) is False
    assert proxy.blackout_active_at(100000) is False


def test_blackout_requires_a_positive_duration() -> None:
    try:
        AckDropProxy(
            12731,
            12732,
            "rtos-to-linux",
            4,
            1,
            "none",
            StringIO(),
            blackout_start_ms=1000,
            blackout_duration_ms=0,
        )
    except ValueError as error:
        assert "blackout_duration_ms" in str(error)
    else:
        raise AssertionError("expected ValueError for zero blackout duration")


def test_fault_delta_accepts_exactly_one_dropped_ack() -> None:
    delivered = Counter({
        ("10.0.42.2", "10.0.42.15", 4, 0, 1): 1,
    })
    source = Counter({
        ("10.0.42.2", "10.0.42.15", 4, 0, 1): 2,
    })

    assert check_expected_fault_delta(
        delivered,
        source,
        "10.0.42.2",
        "10.0.42.15",
        4,
        1,
        1,
    ) == []
