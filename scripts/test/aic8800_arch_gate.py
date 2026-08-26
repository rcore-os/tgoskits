#!/usr/bin/env python3
"""Reject OS/runtime coupling in the portable AIC crate."""

from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / "drivers/net/aic8800"
FORBIDDEN_DEPENDENCIES = ("ax-sync", "rd-net", "ax-task", "axruntime")
SOURCE_FORBIDDEN = (
    "thread::spawn", "thread::sleep", "yield_now(", "sleep_ms(",
    "set_runtime", "WifiRuntime", "AtomicPtr",
)


def main() -> int:
    findings: list[str] = []
    manifest_path = CRATE / "Cargo.toml"
    manifest = manifest_path.read_text(encoding="utf-8")
    for dependency in FORBIDDEN_DEPENDENCIES:
        if dependency in manifest:
            findings.append(f"{manifest_path}: forbidden dependency {dependency}")
    for dependency in ("dma-api", "rdif-eth", "ringbuf", "sdmmc-host"):
        declaration = next(
            (line for line in manifest.splitlines() if line.startswith(f"{dependency} =")),
            "",
        )
        if "optional = true" not in declaration:
            findings.append(
                f"{manifest_path}: RDIF dependency {dependency} must remain optional"
            )
    legacy = ROOT / "drivers/net/aic8800-rdif/Cargo.toml"
    if legacy.exists():
        findings.append(f"{legacy}: obsolete adapter crate must not be retained")
    for source in (CRATE / "src").rglob("*.rs"):
        text = source.read_text(encoding="utf-8")
        for needle in SOURCE_FORBIDDEN:
            if needle in text:
                findings.append(f"{source}: forbidden source token {needle}")
    if findings:
        print("\n".join(findings))
        return 1
    print("AIC8800 portable dependency and source gates passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
