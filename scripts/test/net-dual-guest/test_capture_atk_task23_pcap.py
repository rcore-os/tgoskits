import importlib.util
import struct
from pathlib import Path

import pytest


CAPTURE_PATH = (
    Path(__file__).resolve().parents[2] / "board/capture-atk-task23-pcap.py"
)


def load_capture_module():
    spec = importlib.util.spec_from_file_location("capture_atk_task23_pcap", CAPTURE_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_capture_dump_is_split_into_two_classic_pcaps(tmp_path: Path) -> None:
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
    paths = capture.write_pcaps(tmp_path / "task23", records)

    assert [path.name for path in paths] == ["task23.vm1.pcap", "task23.vm2.pcap"]
    for path, expected_payload in zip(paths, (b"\x00\x11\x22", b"\xaa\xbb\xcc\xdd")):
        data = path.read_bytes()
        assert data[:24] == capture.PCAP_GLOBAL_HEADER
        _, _, captured_length, original_length = struct.unpack("<IIII", data[24:40])
        assert captured_length == original_length == len(expected_payload)
        assert data[40:] == expected_payload


@pytest.mark.parametrize(
    "dump, message",
    (
        (b"CAPTURE 1 1 00", "markers"),
        (b"CAPDUMP_BEGIN\nCAPTURE 1 1 00\nCAPDUMP_END", "VM 2"),
        (
            b"CAPDUMP_BEGIN\nCAPTURE 3 1 00\nCAPTURE 2 2 00\nCAPDUMP_END",
            "unexpected VM",
        ),
    ),
)
def test_capture_dump_rejects_incomplete_or_wrong_guest_evidence(
    dump: bytes, message: str
) -> None:
    capture = load_capture_module()

    with pytest.raises(ValueError, match=message):
        capture.parse_capture_dump(dump)
