#!/usr/bin/env python3
"""Exercise the CI event reader with real sockets and controlled frame delivery."""

import importlib.util
import json
import socket
import time
import unittest
from pathlib import Path
from unittest.mock import patch


PROBE_PATH = (
    Path(__file__).resolve().parents[2]
    / "test-suit/axvisor/qemu/aarch64/general/probes/pool.py"
)
SPEC = importlib.util.spec_from_file_location("pool_http_probe", PROBE_PATH)
probe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(probe)

WEB_SPEC = importlib.util.spec_from_file_location(
    "dashboard_http_probe",
    Path(__file__).resolve().parents[2] / "test-suit/axvisor/qemu/common/web-ui/http_probe.py",
)
dashboard = importlib.util.module_from_spec(WEB_SPEC)
WEB_SPEC.loader.exec_module(dashboard)


def text_frame(body):
    payload = json.dumps(body).encode("utf-8")
    return bytes([0x81, len(payload)]) + payload


class EventReaderTests(unittest.TestCase):
    def test_dashboard_consumes_buffered_snapshot_and_bounds_partial_frames(self):
        client, _server = self.socket_pair()
        snapshot = {"type": "snapshot", "vms": []}
        stream = dashboard.WebSocket(client, text_frame(snapshot) + b"\x81")
        self.assertEqual(dashboard.receive_text(stream, time.monotonic() + 0.05), snapshot)
        client.settimeout(10)
        started = time.monotonic()
        with self.assertRaises((AssertionError, TimeoutError)):
            dashboard.receive_text(stream, started + 0.05)
        self.assertLess(time.monotonic() - started, 1)

    def socket_pair(self):
        client, server = socket.socketpair()
        self.addCleanup(client.close)
        self.addCleanup(server.close)
        client.settimeout(0.5)
        return client, server

    def test_reads_handshake_snapshot_and_next_buffered_event(self):
        client, server = self.socket_pair()
        snapshot = {"type": "snapshot", "vms": []}
        event = {"type": "created", "id": 7}
        server.sendall(
            b"HTTP/1.1 101 Switching Protocols\r\n"
            b"Upgrade: websocket\r\nConnection: Upgrade\r\n\r\n"
            + text_frame(snapshot)
            + text_frame(event)
        )
        with patch.object(probe.socket, "create_connection", return_value=client):
            websocket = probe.open_websocket("/ws/events")

        for expected in (snapshot, event):
            self.assertEqual(
                probe.read_json_frame(websocket, time.monotonic() + 0.05),
                expected,
            )

    def test_reads_frames_split_between_buffer_and_socket(self):
        expected = {"type": "removed", "id": 7}
        frame = text_frame(expected)
        for split in (0, 1, 2, len(frame) - 1):
            with self.subTest(split=split):
                client, server = self.socket_pair()
                server.sendall(frame[split:])
                websocket = probe.WebSocket(client, frame[:split])
                self.assertEqual(
                    probe.read_json_frame(websocket, time.monotonic() + 1),
                    expected,
                )

    def test_incomplete_frames_report_the_read_deadline(self):
        frame = text_frame({"type": "snapshot", "vms": []})
        for prefix in (b"", frame[:1], frame[:-1]):
            with self.subTest(prefix_length=len(prefix)):
                client, server = self.socket_pair()
                if prefix:
                    server.sendall(prefix)
                websocket = probe.WebSocket(client, b"")
                with self.assertRaisesRegex(TimeoutError, "WebSocket frame"):
                    probe.read_json_frame(websocket, time.monotonic() + 0.05)


if __name__ == "__main__":
    unittest.main()
