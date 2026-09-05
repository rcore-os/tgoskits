"""Shared constants and utilities for net-bench Python scripts.

Import from this module to avoid duplicating test labels, byte formatting,
and data types across ``summarize.py`` and ``compare-baseline.py``.
"""

from __future__ import annotations

from dataclasses import dataclass

# ---- Test identity ----------------------------------------------------------

TEST_ORDER: list[str] = ["tcp1", "tcp4", "tcp1r", "udp1g", "udp64"]

TEST_LABELS: dict[str, str] = {
    "tcp1": "TCP 1-stream (uplink)",
    "tcp4": "TCP 4-stream (uplink)",
    "tcp1r": "TCP 1-stream (reverse/downlink)",
    "udp1g": "UDP 1G target (large packets)",
    "udp64": "UDP 64B small-packet PPS",
}

# Test IDs whose traffic direction is reverse (downlink: host → guest).
_REVERSE_TEST_IDS: frozenset[str] = frozenset({"tcp1r"})

# Inverse mapping: display label → test_id (for parsing summary output).
# The "**total**" sentinel label is added explicitly — it has no entry in
# TEST_LABELS but always appears as the last row of the markdown table.
LABEL_TO_ID: dict[str, str] = {v: k for k, v in TEST_LABELS.items()}
LABEL_TO_ID["**total**"] = "**total**"


# ---- /proc/net/dev row format -----------------------------------------------

@dataclass
class NetDevRow:
    """One row of the per-interface per-test /proc/net/dev table.

    Used by ``compare-baseline.py`` to represent parsed summary output.
    """

    test_label: str  # e.g. "tcp1" or "**total**"
    tx_bytes: int
    tx_pkts: int
    rx_bytes: int
    rx_pkts: int
    tx_err: int
    tx_drop: int
    rx_err: int
    rx_drop: int


# ---- Byte formatting --------------------------------------------------------

# Maximum representable delta value before precision loss from floating-point
# division exceeds 0.01% of the true value (acceptable for comparison).
_MAX_SAFE_BYTES = 1 << 50  # ~1 PB — well above any realistic test run


def format_bytes(n: int) -> str:
    """Format a byte count in human-readable form (KB/MB/GB).

    >>> format_bytes(0)
    '0 B'
    >>> format_bytes(1024)
    '1.00 KB'
    >>> format_bytes(203310448)
    '193.89 MB'
    """
    if n >= 1 << 30:
        return f"{n / (1 << 30):.2f} GB"
    if n >= 1 << 20:
        return f"{n / (1 << 20):.2f} MB"
    if n >= 1 << 10:
        return f"{n / (1 << 10):.2f} KB"
    return f"{n} B"


def parse_bytes(val: str) -> int:
    """Parse a human-readable byte count like ``'193.89 MB'`` into an int.

    Handles GB/MB/KB/B units and plain integers.  Commas are stripped
    before conversion.  Returns 0 for unparseable input and emits a
    warning to stderr.
    """
    val = val.strip().replace(",", "")
    if not val:
        return 0
    parts = val.split()
    try:
        if len(parts) == 2:
            num, unit = float(parts[0]), parts[1]
            if num < 0:
                import sys

                print(
                    f"warning: negative byte count in parse_bytes({val!r}), returning 0",
                    file=sys.stderr,
                )
                return 0
            if unit == "GB":
                return int(num * (1 << 30))
            if unit == "MB":
                return int(num * (1 << 20))
            if unit == "KB":
                return int(num * (1 << 10))
            if unit == "B":
                return int(num)
            # Unknown unit — fall through to warning.
        else:
            n = int(float(val))
            if n < 0:
                import sys

                print(
                    f"warning: negative byte count in parse_bytes({val!r}), returning 0",
                    file=sys.stderr,
                )
                return 0
            return n
    except (ValueError, OverflowError):
        pass

    import sys

    print(
        f"warning: cannot parse byte value {val!r}, treating as 0",
        file=sys.stderr,
    )
    return 0
