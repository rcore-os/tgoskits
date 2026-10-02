#!/usr/bin/env python3
"""Guard the layered edges of the Axvisor control plane.

`os/axvisor/src/control` has four layers: `capability` (which request paths
exist), `transport` (the handlers behind them), `web` (the bundle the handlers
serve) and `domain` (the VM registry and the configuration pool the handlers
call). The permitted edges are one-way and few:

    web         -> capability     capability -> transport
    transport   -> domain         capability -> domain

Nothing else in the toolchain can check that. The edges are `pub(crate)`, so
every sibling module can reach every other one and a wrong direction compiles;
`cargo fmt`, `cargo clippy` and every host test stay green either way; and
`axvisor` is a bare-metal kernel that the host toolchain never builds. Only
reading the sources shows who names whom.

Two deliberate relaxations, both about what a token *means* rather than about
which file it sits in:

  - comments do not count as a dependency. `transport/server.rs` links to
    `crate::control::capability::table` in its module docs; that is navigation
    offered to a reader, not the transport layer reaching for the table.
  - a request path is a *string literal* spelling `/api...` or `/ws...`. Prose
    that quotes one does not declare a route.

`RETIRED_PATHS` is the exception that proves the rule: a module path that no
longer resolves is wrong even in a comment, so that gate reads the raw text.

The same contract has a frontend half. The dashboard asks the manifest where
every operation lives, so `web-ui/src` may not spell a request path either; the
only exception is the bootstrap path, which is fixed on both sides and whose two
constants this check compares (one regex per language, because no compiler sees
both).

This file is only as strong as the command that runs it. It is wired into the
always-run static check (`.github/ci/checks/static.toml`) on purpose: the
incremental `cargo xtask test --since` derives its package set from changed
paths, a change under `os/axvisor/src` selects `axvisor`, and `axvisor` cannot
host a host-side test at all. `check_scan_coverage` closes the other half of
that hole by asserting that the path gate read every Rust file of the crate, so
a rename or a new module fails the check instead of quietly escaping it.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
AXVISOR = WORKSPACE_ROOT / "os" / "axvisor"

# `xtask/` next to these is a separate helper package for the Axvisor CLI, not
# part of the kernel crate this check guards, so it stays out of the gates.
CRATE_SOURCES = ("build.rs", "src", "tests")

# (rule, owner directories relative to os/axvisor, module paths that may not be
#  named in code). Owners are disjoint from every other gate's owners, so a
#  finding has exactly one explanation.
CODE_EDGES = (
    (
        "transport and web must not depend on the capability layer",
        ("src/control/transport", "src/control/web"),
        (
            "crate::control::capability",
            "super::capability",
            "super::super::capability",
        ),
    ),
    (
        "the domain must not depend on the layers above it",
        ("src/control/domain",),
        (
            "crate::control::capability",
            "crate::control::transport",
            "crate::control::web",
            "super::capability",
            "super::transport",
            "super::web",
        ),
    ),
)

# (rule, scanned roots relative to os/axvisor, directories allowed to declare
#  paths, literal prefixes that mean "this is a request path"). The capability
#  layer is the single place that knows a URL, which is what lets the router
#  and the manifest be built from the same table.
PATH_LITERALS = (
    (
        "request paths are declared once, in the capability layer",
        CRATE_SOURCES,
        ("src/control/capability",),
        ("/api", "/ws"),
    ),
)

# (rule, scanned roots relative to os/axvisor, text that must not survive at
#  all). These paths stopped resolving when the control plane moved under
#  `control/`; a stale one misleads whether it sits in code or in a comment.
RETIRED_PATHS = (
    (
        "retired module paths must not come back",
        CRATE_SOURCES,
        (
            "crate::http",
            "crate::vm_pool",
            "crate::vm_events",
            "crate::web",
            "src/http/",
            "src/vm_pool.rs",
            "src/vm_events.rs",
            "src/web/",
        ),
    ),
)

# The frontend half of the same contract. The dashboard now reads every
# operation out of the manifest, so its sources must not spell a request path
# either: a path in a panel is a second declaration that the backend cannot see
# and that no route rename would update. Two exemptions, both deliberate: the
# bootstrap path (a client needs one fixed path before it can read anything, and
# it is checked against the Rust constant below) and the unit tests, which pin
# the accessor against real paths and never ship in the bundle.
WEBUI_ROOT = "web-ui/src"
WEBUI_ALLOWED = ("web-ui/src/capability/manifest.ts",)
WEBUI_TEST_SUFFIX = ".test."

# The bootstrap pair: the descriptor cannot say where the descriptor lives, so
# that one path is fixed on both sides. Two constants that have to agree are
# exactly the kind of pair that drifts, and neither language can see the other.
BOOTSTRAP_PAIR = (
    "the bootstrap path is fixed on both sides and the two must match",
    (
        "src/control/capability/mod.rs",
        re.compile(r'MANIFEST_PATH:\s*&str\s*=\s*"([^"]+)"'),
    ),
    (
        "web-ui/src/capability/manifest.ts",
        re.compile(r"MANIFEST_PATH\s*=\s*'([^']+)'"),
    ),
)

IDENT = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_")
RAW_STRING = re.compile(r'b?r(#*)"')
CHAR_LITERAL = re.compile(r"'(?:\\.|[^\\'])'")


def position(text: str, offset: int) -> tuple[int, int]:
    """Translate a byte offset in `text` into a 1-based line and column."""
    line = text.count("\n", 0, offset) + 1
    column = offset - text.rfind("\n", 0, offset)
    return line, column


def code_only(text: str) -> str:
    """Blank out comments, keeping every other offset where it was.

    Strings are consumed whole so that a `//` inside one is not a comment and a
    comment inside one is not reported, which is the difference this check
    depends on: `use crate::control::capability;` is an edge, a doc link with
    the same words is not.
    """
    blanked = list(text)
    index = 0
    length = len(text)
    while index < length:
        char = text[index]
        if char == "/" and index + 1 < length:
            following = text[index + 1]
            if following == "/":
                end = text.find("\n", index)
                end = length if end < 0 else end
                blanked[index:end] = " " * (end - index)
                index = end
                continue
            if following == "*":
                # Rust block comments nest, and may contain `//` or a quote.
                depth = 1
                end = index + 2
                while end < length and depth:
                    if text.startswith("/*", end):
                        depth += 1
                        end += 2
                    elif text.startswith("*/", end):
                        depth -= 1
                        end += 2
                    else:
                        end += 1
                for offset in range(index, min(end, length)):
                    if blanked[offset] != "\n":
                        blanked[offset] = " "
                index = end
                continue
        if char in "br":
            raw = RAW_STRING.match(text, index)
            if raw:
                end = text.find('"' + raw.group(1), raw.end())
                index = length if end < 0 else end + 1 + len(raw.group(1))
                continue
        if char == '"':
            end = index + 1
            while end < length:
                if text[end] == "\\":
                    end += 2
                    continue
                if text[end] == '"':
                    end += 1
                    break
                end += 1
            index = end
            continue
        if char == "'":
            literal = CHAR_LITERAL.match(text, index)
            if literal:
                index = literal.end()
                continue
        index += 1
    return "".join(blanked)


def string_literals(text: str) -> list[tuple[int, str]]:
    """Return every string literal in `text` as (offset of its opening quote, value)."""
    literals: list[tuple[int, str]] = []
    blanked = code_only(text)
    index = 0
    length = len(text)
    while index < length:
        char = text[index]
        if char in "br":
            raw = RAW_STRING.match(text, index)
            if raw:
                end = text.find('"' + raw.group(1), raw.end())
                if end < 0:
                    break
                literals.append((end, text[raw.end() : end]))
                index = end + 1 + len(raw.group(1))
                continue
        if char == '"' and blanked[index] == '"':
            end = index + 1
            while end < length:
                if text[end] == "\\":
                    end += 2
                    continue
                if text[end] == '"':
                    break
                end += 1
            literals.append((index, text[index + 1 : end]))
            index = end + 1
            continue
        if char == "'":
            literal = CHAR_LITERAL.match(text, index)
            if literal and blanked[literal.start()] == "'":
                index = literal.end()
                continue
        index += 1
    return literals


def occurrences(text: str, token: str) -> list[int]:
    """Find `token` where it stands alone as a Rust path, not as a prefix."""
    found: list[int] = []
    start = text.find(token)
    while start >= 0:
        before = text[start - 1] if start else ""
        end = start + len(token)
        after = text[end] if end < len(text) else ""
        if before not in IDENT and after not in IDENT:
            found.append(start)
        start = text.find(token, start + 1)
    return found


def sources(relative: str) -> list[Path]:
    """Every Rust file under a crate-relative file name or directory."""
    target = AXVISOR / relative
    if target.is_file():
        return [target]
    return sorted(path for path in target.rglob("*.rs") if path.is_file())


def webui_sources(relative: str) -> list[Path]:
    """Every TypeScript source under a web-ui-relative file name or directory."""
    target = AXVISOR / relative
    if target.is_file():
        return [target]
    return sorted(
        path
        for suffix in ("*.ts", "*.tsx")
        for path in target.rglob(suffix)
        if path.is_file()
    )


def ts_literals(text: str) -> list[tuple[int, str]]:
    """Return every string literal of a TypeScript source as (offset, value).

    Comments are skipped so that a path quoted in prose is not a declaration,
    and all three quote forms count: a template literal is a string here, which
    is what makes a deliberately built path visible. The scan is allowed to be
    conservative in one direction only: a stray quote in JSX text can swallow
    text until the next one, which loses findings but never invents them.
    """
    literals: list[tuple[int, str]] = []
    index = 0
    length = len(text)
    while index < length:
        char = text[index]
        if char == "/" and index + 1 < length:
            following = text[index + 1]
            if following == "/":
                end = text.find("\n", index)
                index = length if end < 0 else end
                continue
            if following == "*":
                end = text.find("*/", index + 2)
                index = length if end < 0 else end + 2
                continue
        if char in "\"'`":
            end = index + 1
            while end < length:
                if text[end] == "\\":
                    end += 2
                    continue
                if text[end] == char:
                    break
                end += 1
            literals.append((index, text[index + 1 : end]))
            index = end + 1
            continue
        index += 1
    return literals


def crate_sources() -> set[Path]:
    """Every Rust file of the kernel crate, `xtask/` excluded."""
    everything = {path for path in AXVISOR.rglob("*.rs") if path.is_file()}
    return {
        path for path in everything if path.relative_to(AXVISOR).parts[0] != "xtask"
    }


def relative_to_workspace(path: Path) -> str:
    return path.relative_to(WORKSPACE_ROOT).as_posix()


def check_code_edges(findings: list[str]) -> None:
    """Report module paths that point the wrong way, ignoring comments."""
    for rule, owners, forbidden in CODE_EDGES:
        longest_first = sorted(forbidden, key=len, reverse=True)
        for owner in owners:
            for source in sources(owner):
                text = source.read_text(encoding="utf-8")
                code = code_only(text)
                reported: set[int] = set()
                for token in longest_first:
                    for offset in occurrences(code, token):
                        if offset in reported:
                            continue
                        reported.add(offset)
                        line, column = position(text, offset)
                        findings.append(
                            f"{relative_to_workspace(source)}:{line}:{column}: {rule}"
                            f" (names {token})"
                        )


def check_path_literals(findings: list[str], seen: set[Path]) -> None:
    """Report request paths declared outside the capability layer."""
    for rule, roots, allowed, prefixes in PATH_LITERALS:
        permitted = [AXVISOR / directory for directory in allowed]
        for root in roots:
            for source in sources(root):
                seen.add(source)
                if any(source.is_relative_to(directory) for directory in permitted):
                    continue
                text = source.read_text(encoding="utf-8")
                for offset, value in string_literals(text):
                    if not any(
                        value == prefix or value.startswith(prefix + "/")
                        for prefix in prefixes
                    ):
                        continue
                    line, column = position(text, offset)
                    findings.append(
                        f"{relative_to_workspace(source)}:{line}:{column}: {rule}"
                        f" (declares {value})"
                    )


def check_webui_paths(findings: list[str], seen: set[Path]) -> None:
    """Report request paths declared in the dashboard, comments excluded."""
    permitted = [AXVISOR / allowed for allowed in WEBUI_ALLOWED]
    for source in webui_sources(WEBUI_ROOT):
        seen.add(source)
        if any(source.is_relative_to(allowed) for allowed in permitted):
            continue
        if WEBUI_TEST_SUFFIX in source.name:
            continue
        text = source.read_text(encoding="utf-8")
        for offset, value in ts_literals(text):
            if not any(
                value == prefix or value.startswith(prefix + "/")
                for prefix in ("/api", "/ws")
            ):
                continue
            line, column = position(text, offset)
            findings.append(
                f"{relative_to_workspace(source)}:{line}:{column}: "
                "the dashboard reads its paths from the manifest"
                f" (declares {value})"
            )


def check_bootstrap_pair(errors: list[str]) -> None:
    """Compare the two halves of the one path that is fixed on both sides."""
    rule, *sides = BOOTSTRAP_PAIR
    declared: list[str] = []
    for relative, pattern in sides:
        source = AXVISOR / relative
        if not source.is_file():
            errors.append(f"{rule}: os/axvisor/{relative} is gone")
            return
        text = source.read_text(encoding="utf-8")
        found = pattern.search(code_only(text) if relative.endswith(".rs") else text)
        if not found:
            errors.append(f"{rule}: os/axvisor/{relative} no longer declares it")
            return
        declared.append(found.group(1))
    if len(set(declared)) != 1:
        errors.append(f"{rule}: {' vs '.join(declared)}")


def check_retired_paths(findings: list[str]) -> None:
    """Report module paths that stopped resolving, comments included."""
    for rule, roots, retired in RETIRED_PATHS:
        for root in roots:
            for source in sources(root):
                text = source.read_text(encoding="utf-8")
                reported: set[int] = set()
                for token in sorted(retired, key=len, reverse=True):
                    for offset in occurrences(text, token):
                        if offset in reported:
                            continue
                        reported.add(offset)
                        line, column = position(text, offset)
                        findings.append(
                            f"{relative_to_workspace(source)}:{line}:{column}: {rule}"
                            f" (mentions {token})"
                        )


def check_scan_coverage(errors: list[str], seen: set[Path], seen_web: set[Path]) -> None:
    """Every gate must still cover something, or it silently stops working."""
    for rule, owners, _ in CODE_EDGES:
        for owner in owners:
            if not (AXVISOR / owner).is_dir():
                errors.append(f"{rule}: os/axvisor/{owner} is gone")
            elif not sources(owner):
                errors.append(f"{rule}: os/axvisor/{owner} holds no Rust source")
    for rule, roots, allowed, _ in PATH_LITERALS:
        for root in roots:
            if not (AXVISOR / root).exists():
                errors.append(f"{rule}: os/axvisor/{root} is gone")
        for directory in allowed:
            if not (AXVISOR / directory).is_dir():
                errors.append(f"{rule}: os/axvisor/{directory} is gone")
    for uncovered in sorted(crate_sources() - seen):
        errors.append(
            "the path gate did not read os/axvisor/"
            f"{uncovered.relative_to(AXVISOR).as_posix()}: add it to CRATE_SOURCES"
        )
    if not (AXVISOR / WEBUI_ROOT).is_dir():
        errors.append(f"the dashboard source tree is gone: os/axvisor/{WEBUI_ROOT}")
    for uncovered in sorted(set(webui_sources(WEBUI_ROOT)) - seen_web):
        errors.append(
            "the frontend path gate did not read os/axvisor/"
            f"{uncovered.relative_to(AXVISOR).as_posix()}"
        )
    allowed_read = [
        source
        for source in seen_web
        if any(source.is_relative_to(AXVISOR / allowed) for allowed in WEBUI_ALLOWED)
        and any(
            value == "/api" or value.startswith("/api/")
            for _, value in ts_literals(source.read_text(encoding="utf-8"))
        )
    ]
    if not allowed_read:
        errors.append(
            "the frontend path gate exempted nothing: "
            f"{', '.join(WEBUI_ALLOWED)} no longer declares the bootstrap path"
        )


def main() -> int:
    if not AXVISOR.is_dir():
        print(f"missing source tree: {AXVISOR}", file=sys.stderr)
        return 2
    errors: list[str] = []
    findings: list[str] = []
    seen: set[Path] = set()
    seen_web: set[Path] = set()
    check_code_edges(findings)
    check_path_literals(findings, seen)
    check_webui_paths(findings, seen_web)
    check_retired_paths(findings)
    check_bootstrap_pair(errors)
    check_scan_coverage(errors, seen, seen_web)
    errors.extend(f"control layer violation {finding}" for finding in findings)
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        return 1
    print(f"AXVISOR_CONTROL_LAYERS_PASSED files={len(seen)} web={len(seen_web)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
