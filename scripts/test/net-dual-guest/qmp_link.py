#!/usr/bin/env python3
"""Control a QEMU netdev link and shut down an instance through QMP.

The ``on``/``off`` form injects a data-link transition.  The special
``quit`` form performs the normal QEMU shutdown required by the evidence
workflow; it is deliberately implemented over the same QMP client so command
responses and asynchronous events cannot be confused.
"""

from __future__ import annotations

import argparse
import json
import socket
import sys
import time


class QmpError(RuntimeError):
    """QMP returned an error response."""


class QmpClient:
    """Line-buffered QMP transport preserving responses coalesced by recv()."""

    def __init__(self, stream: socket.socket) -> None:
        self.stream = stream
        self.buffer = bytearray()

    def receive_json(self) -> dict[str, object]:
        while True:
            separator = self.buffer.find(b"\r\n")
            separator_len = 2
            if separator < 0:
                separator = self.buffer.find(b"\n")
                separator_len = 1
            if separator >= 0:
                line = bytes(self.buffer[:separator])
                del self.buffer[: separator + separator_len]
                break
            if len(self.buffer) >= 64 * 1024:
                raise QmpError("QMP response exceeds 64 KiB")
            chunk = self.stream.recv(4096)
            if not chunk:
                raise QmpError("QMP closed before a complete response")
            self.buffer.extend(chunk)
        try:
            return json.loads(line.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise QmpError(f"invalid QMP response: {line!r}") from error


    def execute(
        self, command: str, arguments: dict[str, object] | None = None
    ) -> dict[str, object]:
        request: dict[str, object] = {"execute": command}
        if arguments:
            request["arguments"] = arguments
        self.stream.sendall(json.dumps(request).encode("utf-8") + b"\r\n")
        while True:
            response = self.receive_json()
            if "event" in response:
                continue
            if "error" in response:
                raise QmpError(str(response["error"]))
            return response


def run(path: str, device: str, state: str | None, wait_ms: int) -> None:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(5)
        stream.connect(path)
        client = QmpClient(stream)
        greeting = client.receive_json()
        if "QMP" not in greeting:
            raise QmpError("QMP greeting missing")
        client.execute("qmp_capabilities")
        if device == "quit":
            if state is not None:
                raise QmpError("quit does not accept a link state")
            result = client.execute("quit")
            output = {"action": "quit", "result": result}
        else:
            if state not in ("on", "off"):
                raise QmpError("link control requires state 'on' or 'off'")
            result = client.execute("set_link", {"name": device, "up": state == "on"})
            output = {"device": device, "state": state, "result": result}
        print(json.dumps(output, sort_keys=True))
    if wait_ms:
        time.sleep(wait_ms / 1000)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("socket")
    parser.add_argument(
        "device",
        help="QEMU netdev id, for example net-rtos; use 'quit' to shut down",
    )
    parser.add_argument("state", choices=("on", "off"), nargs="?")
    parser.add_argument("--wait-ms", type=int, default=0)
    args = parser.parse_args()
    if args.device == "quit" and args.state is not None:
        parser.error("quit does not accept a state")
    if args.device != "quit" and args.state is None:
        parser.error("a link state is required unless device is 'quit'")
    try:
        run(args.socket, args.device, args.state, args.wait_ms)
    except (OSError, QmpError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
