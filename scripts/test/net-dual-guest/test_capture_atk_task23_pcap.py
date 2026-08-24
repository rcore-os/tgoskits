import importlib.util
import struct
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock


CAPTURE_PATH = (
    Path(__file__).resolve().parents[2] / "board/capture-atk-task23-pcap.py"
)


def load_capture_module():
    spec = importlib.util.spec_from_file_location("capture_atk_task23_pcap", CAPTURE_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    with mock.patch.dict(sys.modules, {"serial": types.ModuleType("serial")}):
        spec.loader.exec_module(module)
    return module


class CaptureAtKTask23PcapTests(unittest.TestCase):
    def test_capture_dump_is_split_into_two_classic_pcaps(self) -> None:
        capture = load_capture_module()
        dump = b"\n".join(
            (
                b"noise before marker",
                b"CAPDUMP_BEGIN",
                b"CAPTURE 1 1000000123 001122",
                b"CAPTURE 2 2000000456 aabbccdd",
                b"CAPDUMP_END",
                b"noise after marker",
            )
        )

        records = capture.parse_capture_dump(dump)
        with tempfile.TemporaryDirectory() as temp_dir:
            paths = capture.write_pcaps(Path(temp_dir) / "task23", records)

            self.assertEqual(
                [path.name for path in paths],
                ["task23.vm1.pcap", "task23.vm2.pcap"],
            )
            for path, expected_payload in zip(
                paths, (b"\x00\x11\x22", b"\xaa\xbb\xcc\xdd")
            ):
                data = path.read_bytes()
                self.assertEqual(data[:24], capture.PCAP_GLOBAL_HEADER)
                _, _, captured_length, original_length = struct.unpack(
                    "<IIII", data[24:40]
                )
                self.assertEqual(captured_length, original_length)
                self.assertEqual(captured_length, len(expected_payload))
                self.assertEqual(data[40:], expected_payload)

    def test_capture_dump_rejects_incomplete_or_wrong_guest_evidence(self) -> None:
        capture = load_capture_module()
        cases = (
            (b"CAPTURE 1 1 00", "markers"),
            (b"CAPDUMP_BEGIN\nCAPTURE 1 1 00\nCAPDUMP_END", "VM 2"),
            (
                b"CAPDUMP_BEGIN\nCAPTURE 3 1 00\nCAPTURE 2 2 00\nCAPDUMP_END",
                "unexpected VM",
            ),
        )

        for dump, message in cases:
            with self.subTest(message=message):
                with self.assertRaisesRegex(ValueError, message):
                    capture.parse_capture_dump(dump)
