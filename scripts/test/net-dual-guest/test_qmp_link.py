#!/usr/bin/env python3
"""Regression test for coalesced QMP greeting/response reads."""

from __future__ import annotations

import importlib.util
import socket
import threading
import unittest
from pathlib import Path


SPEC = importlib.util.spec_from_file_location(
    "qmp_link", Path(__file__).with_name("qmp_link.py")
)
assert SPEC and SPEC.loader
QMP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(QMP)


class QmpLinkTests(unittest.TestCase):
    def test_coalesced_lines_are_not_dropped(self) -> None:
        server, client_socket = socket.socketpair()

        def serve() -> None:
            server.sendall(
                b'{"QMP":{"version":{}}}\r\n'
                b'{"return":{}}\r\n'
                b'{"return":{"up":true}}\r\n'
            )
            server.recv(4096)
            server.close()

        thread = threading.Thread(target=serve)
        thread.start()
        client = QMP.QmpClient(client_socket)
        self.assertIn("QMP", client.receive_json())
        self.assertEqual(client.execute("qmp_capabilities"), {"return": {}})
        self.assertEqual(
            client.receive_json(), {"return": {"up": True}}
        )
        client_socket.close()
        thread.join()


if __name__ == "__main__":
    unittest.main()
