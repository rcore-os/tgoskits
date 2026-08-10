#!/usr/bin/env python3
"""Relay QEMU socket-netdev frames while injecting a bounded T2N1 drop.

QEMU's stream socket backend prefixes every Ethernet frame with a four-byte
big-endian length.  The proxy preserves that framing and inspects only the
Ethernet/IPv4/UDP payload, so the injected loss occurs on the real Guest link
between the two QEMU netdevs rather than inside either Guest protocol stack.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import socket
import sys
import threading
from typing import TextIO

from task2_responder import CONTROL_KIND, SESSION_ID, encode_frame


FRAME_LENGTH_BYTES = 4
MAX_FRAME_LENGTH = 1 << 20
ETHERNET_HEADER_LENGTH = 14
IPV4_PROTOCOL_OFFSET = 9
UDP_HEADER_LENGTH = 8
T2N1_MAGIC = b"T2N1"
T2N1_KIND_OFFSET = 5
T2N1_SEQUENCE_OFFSET = 12

MESSAGE_KINDS = {
    "control": 1,
    "status": 2,
    "error": 3,
    "ack": 4,
    "heartbeat": 5,
}
INJECTION_MODES = ("none", "out-of-order", "invalid-parameter")
LINUX_MAC = bytes.fromhex("525400123401")
RTOS_MAC = bytes.fromhex("525400123402")
LINUX_IP = bytes((10, 0, 42, 15))
RTOS_IP = bytes((10, 0, 42, 2))
UDP_PORT = 4242


@dataclass(frozen=True)
class Task2Metadata:
    """Metadata extracted from one IPv4/UDP T2N1 frame."""

    kind: int
    sequence: int
    acknowledgement: int


class ProxyError(RuntimeError):
    """The proxy cannot preserve or inspect the QEMU stream."""


def task2_metadata(ethernet_frame: bytes) -> Task2Metadata | None:
    """Return T2N1 kind and sequence for an Ethernet frame, if present."""
    if len(ethernet_frame) < ETHERNET_HEADER_LENGTH:
        return None

    network_offset = ETHERNET_HEADER_LENGTH
    ether_type = int.from_bytes(ethernet_frame[12:14], "big")
    if ether_type == 0x8100:
        if len(ethernet_frame) < ETHERNET_HEADER_LENGTH + 4:
            return None
        ether_type = int.from_bytes(ethernet_frame[16:18], "big")
        network_offset += 4
    if ether_type != 0x0800 or len(ethernet_frame) <= network_offset:
        return None

    ip_header_length = (ethernet_frame[network_offset] & 0x0F) * 4
    if ip_header_length < 20:
        return None
    if len(ethernet_frame) < network_offset + ip_header_length + UDP_HEADER_LENGTH:
        return None
    if ethernet_frame[network_offset + IPV4_PROTOCOL_OFFSET] != 17:
        return None

    udp_offset = network_offset + ip_header_length
    payload_offset = udp_offset + UDP_HEADER_LENGTH
    payload = ethernet_frame[payload_offset:]
    if len(payload) <= T2N1_SEQUENCE_OFFSET + 4 or payload[:4] != T2N1_MAGIC:
        return None

    return Task2Metadata(
        kind=payload[T2N1_KIND_OFFSET],
        sequence=int.from_bytes(
            payload[T2N1_SEQUENCE_OFFSET : T2N1_SEQUENCE_OFFSET + 4], "big"
        ),
        acknowledgement=int.from_bytes(payload[16:20], "big"),
    )


def read_exact(stream: socket.socket, length: int) -> bytes:
    """Read exactly ``length`` bytes, tolerating the proxy's poll timeout."""
    result = bytearray()
    while len(result) < length:
        try:
            chunk = stream.recv(length - len(result))
        except socket.timeout:
            continue
        if not chunk:
            raise EOFError("QEMU socket closed while reading a frame")
        result.extend(chunk)
    return bytes(result)


def read_qemu_frame(stream: socket.socket) -> tuple[bytes, bytes]:
    """Read one QEMU length-prefixed frame and return wire prefix and payload."""
    prefix = read_exact(stream, FRAME_LENGTH_BYTES)
    frame_length = int.from_bytes(prefix, "big")
    if frame_length == 0 or frame_length > MAX_FRAME_LENGTH:
        raise ProxyError(f"invalid QEMU frame length {frame_length}")
    return prefix, read_exact(stream, frame_length)


class AckDropProxy:
    """Two-port QEMU stream relay with a finite, direction-specific drop."""

    def __init__(
        self,
        linux_port: int,
        rtos_port: int,
        drop_direction: str,
        drop_kind: int,
        drop_count: int,
        injection_mode: str,
        output: TextIO,
    ) -> None:
        if drop_count < 0:
            raise ValueError("drop_count must not be negative")
        self.linux_port = linux_port
        self.rtos_port = rtos_port
        self.drop_direction = drop_direction
        self.drop_kind = drop_kind
        self.drop_remaining = drop_count
        if injection_mode not in INJECTION_MODES:
            raise ValueError(f"unsupported injection mode {injection_mode!r}")
        self.injection_mode = injection_mode
        self.injected = False
        self.output = output
        self._log_lock = threading.Lock()
        self._stop = threading.Event()
        self.dropped = 0
        self.forwarded = 0

    def serve(self) -> None:
        """Accept both QEMU endpoints and relay until either endpoint closes."""
        with self._listener(self.linux_port) as linux_listener, self._listener(
            self.rtos_port
        ) as rtos_listener:
            self.log(
                "PROXY_READY linux_port={} rtos_port={} drop_direction={} "
                "drop_kind={} drop_count={} injection={}".format(
                    self.linux_port,
                    self.rtos_port,
                    self.drop_direction,
                    self.drop_kind,
                    self.drop_remaining,
                    self.injection_mode,
                )
            )
            linux_stream, _ = linux_listener.accept()
            rtos_stream, _ = rtos_listener.accept()
            linux_stream.settimeout(1.0)
            rtos_stream.settimeout(1.0)
            self.log("PROXY_CONNECTED")
            with linux_stream, rtos_stream:
                threads = [
                    threading.Thread(
                        target=self._relay,
                        args=(linux_stream, rtos_stream, "linux-to-rtos"),
                        daemon=True,
                    ),
                    threading.Thread(
                        target=self._relay,
                        args=(rtos_stream, linux_stream, "rtos-to-linux"),
                        daemon=True,
                    ),
                ]
                for thread in threads:
                    thread.start()
                for thread in threads:
                    thread.join()
        self.log(
            f"PROXY_SUMMARY dropped={self.dropped} forwarded={self.forwarded}"
        )

    def _relay(
        self, source: socket.socket, destination: socket.socket, direction: str
    ) -> None:
        try:
            while not self._stop.is_set():
                prefix, frame = read_qemu_frame(source)
                metadata = task2_metadata(frame)
                if self._should_drop(metadata, direction):
                    self.dropped += 1
                    self.log(
                        "PROXY_DROP direction={} kind={} sequence={} ack={} remaining={}".format(
                            direction,
                            self._kind_name(metadata.kind),
                            metadata.sequence,
                            metadata.acknowledgement,
                            self.drop_remaining,
                        )
                    )
                    continue
                destination.sendall(prefix + frame)
                self.forwarded += 1
                if direction == "linux-to-rtos":
                    self._inject_after_control(metadata, destination)
        except EOFError:
            self.log(f"PROXY_EOF direction={direction}")
        except (OSError, ProxyError) as error:
            if not self._stop.is_set():
                self.log(f"PROXY_ERROR direction={direction} error={error}")
        finally:
            self._stop.set()
            try:
                destination.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

    def _should_drop(
        self, metadata: Task2Metadata | None, direction: str
    ) -> bool:
        if metadata is None:
            return False
        if direction != self.drop_direction or metadata.kind != self.drop_kind:
            return False
        if self.drop_remaining == 0:
            return False
        self.drop_remaining -= 1
        return True

    def _inject_after_control(
        self, metadata: Task2Metadata | None, destination: socket.socket
    ) -> None:
        """Inject one malformed-semantics frame after the first control."""
        if self.injected or self.injection_mode == "none":
            return
        if metadata is None or metadata.kind != MESSAGE_KINDS["control"]:
            return
        if metadata.sequence != 1:
            return

        if self.injection_mode == "out-of-order":
            sequence = 99
            value = 100
        else:
            sequence = 2
            value = 1001
        payload = bytearray(12)
        payload[0] = 1  # SetOutput
        payload[4:8] = value.to_bytes(4, "big", signed=True)
        payload[8:12] = (2).to_bytes(4, "big")
        datagram = encode_frame(
            CONTROL_KIND,
            SESSION_ID,
            sequence,
            0,
            bytes(payload),
            reliable=True,
        )
        frame = ethernet_udp_frame(datagram, LINUX_MAC, RTOS_MAC, LINUX_IP, RTOS_IP)
        destination.sendall(len(frame).to_bytes(FRAME_LENGTH_BYTES, "big") + frame)
        self.injected = True
        self.forwarded += 1
        self.log(
            "PROXY_INJECT mode={} kind=control sequence={} value={}".format(
                self.injection_mode, sequence, value
            )
        )

    @staticmethod
    def _kind_name(kind: int) -> str:
        for name, value in MESSAGE_KINDS.items():
            if value == kind:
                return name
        return f"unknown-{kind}"

    @staticmethod
    def _listener(port: int) -> socket.socket:
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", port))
        listener.listen(1)
        return listener

    def log(self, message: str) -> None:
        timestamp = datetime.now(timezone.utc).isoformat()
        with self._log_lock:
            print(f"{timestamp} {message}", file=self.output, flush=True)


def ethernet_udp_frame(
    payload: bytes,
    source_mac: bytes,
    destination_mac: bytes,
    source_ip: bytes,
    destination_ip: bytes,
) -> bytes:
    """Build a padded Ethernet/IPv4/UDP frame for controlled injection."""
    udp_length = UDP_HEADER_LENGTH + len(payload)
    ip_length = 20 + udp_length
    ip_header = bytearray(20)
    ip_header[0] = 0x45
    ip_header[2:4] = ip_length.to_bytes(2, "big")
    ip_header[8] = 64
    ip_header[9] = 17
    ip_header[12:16] = source_ip
    ip_header[16:20] = destination_ip
    ip_header[10:12] = ipv4_checksum(bytes(ip_header)).to_bytes(2, "big")
    udp_header = UDP_PORT.to_bytes(2, "big") * 2 + udp_length.to_bytes(2, "big") + b"\x00\x00"
    frame = destination_mac + source_mac + b"\x08\x00" + bytes(ip_header) + udp_header + payload
    return frame.ljust(60, b"\x00")


def ipv4_checksum(header: bytes) -> int:
    """Return the one's-complement checksum for an IPv4 header."""
    total = sum(int.from_bytes(header[offset : offset + 2], "big") for offset in range(0, len(header), 2))
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return (~total) & 0xFFFF


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--linux-port", type=int, default=12731)
    parser.add_argument("--rtos-port", type=int, default=12732)
    parser.add_argument(
        "--drop-direction",
        choices=("linux-to-rtos", "rtos-to-linux"),
        default="rtos-to-linux",
    )
    parser.add_argument(
        "--drop-kind",
        choices=tuple(MESSAGE_KINDS),
        default="ack",
    )
    parser.add_argument("--drop-count", type=int, default=1)
    parser.add_argument(
        "--inject",
        choices=INJECTION_MODES,
        default="none",
        help="inject one valid-wire but invalid-semantics control after seq=1",
    )
    parser.add_argument("--log", type=argparse.FileType("w"), default=sys.stdout)
    args = parser.parse_args()
    try:
        proxy = AckDropProxy(
            args.linux_port,
            args.rtos_port,
            args.drop_direction,
            MESSAGE_KINDS[args.drop_kind],
            args.drop_count,
            args.inject,
            args.log,
        )
        proxy.serve()
    except (OSError, ValueError, ProxyError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
