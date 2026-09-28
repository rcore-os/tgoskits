#!/usr/bin/env python3

"""Prepare the published benchmark dashboard for the Docs Pages workflow.

The workflow downloads the ``benchmark-updates`` artifact and forwards the
dispatch inputs. This script owns everything else: it reads the deployed Pages
dashboard (falling back to the frozen ``perf-data`` branch only when both Pages
files return 404), applies the current benchmark increments through
``ci_perf_dashboard.py`` and writes the dashboard into the Pages output
directory. Keeping that logic here leaves the workflow with environment setup
and one explicit script call.
"""

import argparse
import http.client
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

MODULE_DIR = Path(__file__).resolve().parent
if str(MODULE_DIR) not in sys.path:
    sys.path.insert(0, str(MODULE_DIR))

import ci_perf_dashboard

WORKSPACE_ROOT = MODULE_DIR.parents[1]
DEFAULT_TITLE = "Nightly Performance Benchmarks"
DEFAULT_WINDOW = 10
BENCHMARK_SOURCES = ("axvisor", "starry")

LEGACY_COMPLETE = 0
LEGACY_UNAVAILABLE = 1
LEGACY_INCOMPLETE = 2


class PagesError(Exception):
    """Raised when the published or legacy dashboard cannot be prepared."""


@dataclass(frozen=True)
class RemoteFile:
    status: int
    content: bytes | None


@dataclass(frozen=True)
class LegacyBootstrap:
    status: int
    files: dict[str, bytes] = field(default_factory=dict)


@dataclass(frozen=True)
class PagesConfig:
    base_url: str
    output_dir: Path
    updates_dir: Path
    benchmark_run_id: str = ""
    benchmark_revision: str = ""
    benchmark_date: str = ""
    run_id: str = ""
    run_attempt: str = ""
    workspace_root: Path = WORKSPACE_ROOT
    title: str = DEFAULT_TITLE
    window: int = DEFAULT_WINDOW


GitRunner = Callable[[list[str]], tuple[int, bytes]]
FetchFile = Callable[[str, str, str], RemoteFile]


def cache_buster(run_id: str, run_attempt: str) -> str:
    return f"{run_id}-{run_attempt}"


def fetch_published_file(
    base_url: str,
    filename: str,
    buster: str,
    *,
    opener: Callable[[urllib.request.Request], object] = urllib.request.urlopen,
) -> RemoteFile:
    """Fetch one deployed Pages dashboard file.

    A 404 is a normal "not published yet" result; every other non-200 status,
    network error or empty 200 response is fatal.
    """
    url = f"{base_url.rstrip('/')}/benchmark/{filename}?cache_buster={buster}"
    request = urllib.request.Request(url, headers={"Cache-Control": "no-cache"})
    try:
        with opener(request) as response:
            status = int(getattr(response, "status", 200))
            content = response.read()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return RemoteFile(status=404, content=None)
        raise PagesError(f"Failed to fetch {url}: HTTP {error.code}") from error
    except (urllib.error.URLError, http.client.HTTPException, OSError) as error:
        raise PagesError(f"Failed to fetch {url}: {error}") from error

    if status == 404:
        return RemoteFile(status=404, content=None)
    if status != 200:
        raise PagesError(f"Unexpected HTTP status {status} for {url}")
    if not content:
        raise PagesError(f"Published {filename} is empty")
    return RemoteFile(status=200, content=content)


def make_git_runner(workspace_root: Path) -> GitRunner:
    def run_git(args: list[str]) -> tuple[int, bytes]:
        completed = subprocess.run(
            ["git", *args],
            cwd=workspace_root,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        return completed.returncode, completed.stdout

    return run_git


def bootstrap_legacy(
    workspace_root: Path,
    *,
    git: GitRunner | None = None,
) -> LegacyBootstrap:
    """Read the frozen legacy dashboard without ever writing to the branch."""
    run_git = git or make_git_runner(workspace_root)
    code, _ = run_git(["fetch", "--depth=1", "origin", "perf-data"])
    if code != 0:
        return LegacyBootstrap(status=LEGACY_UNAVAILABLE)

    files: dict[str, bytes] = {}
    for name in ("history.json", "index.html"):
        code, output = run_git(["show", f"FETCH_HEAD:{name}"])
        if code == 0 and output:
            files[name] = output
    if len(files) == 2:
        return LegacyBootstrap(status=LEGACY_COMPLETE, files=files)
    if files:
        return LegacyBootstrap(status=LEGACY_INCOMPLETE, files=files)
    return LegacyBootstrap(status=LEGACY_UNAVAILABLE)


def write_dashboard(output_dir: Path, history: bytes, index: bytes) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "history.json").write_bytes(history)
    (output_dir / "index.html").write_bytes(index)


def read_increment(path: Path) -> list[dict[str, object]] | None:
    """Return a non-empty metrics array, or None when there is no increment."""
    if not path.is_file():
        return None
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise PagesError(f"invalid benchmark increment {path}: {error}") from error
    if isinstance(payload, (list, dict)) and not payload:
        return None
    try:
        return ci_perf_dashboard.load_metrics(path)
    except ValueError as error:
        raise PagesError(f"invalid benchmark increment {path}: {error}") from error


def prepare_published_dashboard(
    output_dir: Path,
    history: RemoteFile,
    index: RemoteFile,
    bootstrap: LegacyBootstrap | None,
) -> None:
    if history.status == 200 and index.status == 200:
        write_dashboard(output_dir, history.content or b"", index.content or b"")
        print("Preserved published performance dashboard")
        return

    if history.status == 404 and index.status == 404:
        if bootstrap is not None and bootstrap.status == LEGACY_COMPLETE:
            write_dashboard(
                output_dir,
                bootstrap.files["history.json"],
                bootstrap.files["index.html"],
            )
            print("Bootstrapped performance dashboard from the frozen legacy branch")
            return
        if bootstrap is not None and bootstrap.status == LEGACY_INCOMPLETE:
            raise PagesError("Legacy performance dashboard is incomplete")
        raise PagesError("Legacy performance dashboard is unavailable")

    raise PagesError(
        "Unexpected published dashboard state: "
        f"history={history.status}, index={index.status}"
    )


def prepare_benchmark_update(
    config: PagesConfig,
    output_dir: Path,
    history: RemoteFile,
    index: RemoteFile,
    bootstrap: LegacyBootstrap | None,
) -> None:
    if history.status == 200 and index.status == 200:
        seed_history = history.content or b""
    elif (
        history.status == 404
        and index.status == 404
        and bootstrap is not None
        and bootstrap.status == LEGACY_COMPLETE
    ):
        seed_history = bootstrap.files["history.json"]
        print("Bootstrapping benchmark history from the frozen legacy branch")
    else:
        raise PagesError(
            "Benchmark updates require published or legacy dashboard data"
        )

    if not config.benchmark_revision:
        raise PagesError("benchmark revision must not be empty")
    if not config.benchmark_date:
        raise PagesError("benchmark date must not be empty")

    output_dir.mkdir(parents=True, exist_ok=True)
    history_path = output_dir / "history.json"
    history_path.write_bytes(seed_history)
    # load_history keeps the legacy top-level array readable as AxVisor history.
    merged = ci_perf_dashboard.load_history(history_path)

    updated = False
    for source in BENCHMARK_SOURCES:
        metrics = read_increment(config.updates_dir / f"{source}.json")
        if metrics is None:
            continue
        merged = ci_perf_dashboard.update_history(
            merged,
            config.benchmark_date,
            config.benchmark_revision,
            metrics,
            source,
        )
        updated = True

    if not updated:
        raise PagesError("No benchmark updates were found in the artifact")

    history_path.write_text(json.dumps(merged, indent=2) + "\n", encoding="utf-8")
    (output_dir / "index.html").write_text(
        ci_perf_dashboard.render_dashboard(config.title, merged, config.window),
        encoding="utf-8",
    )
    print(
        f"Updated performance dashboard from benchmark run "
        f"{config.benchmark_run_id}"
    )


def prepare_dashboard(
    config: PagesConfig,
    *,
    fetch_file: FetchFile = fetch_published_file,
    git: GitRunner | None = None,
) -> None:
    if not config.base_url:
        raise PagesError("PAGES_BASE_URL must not be empty")

    buster = cache_buster(config.run_id, config.run_attempt)
    history = fetch_file(config.base_url, "history.json", buster)
    index = fetch_file(config.base_url, "index.html", buster)
    print(f"Online benchmark history status: {history.status}")
    print(f"Online benchmark index status: {index.status}")

    bootstrap = None
    # The legacy branch stays read-only and is only consulted when Pages has
    # never published the two dashboard files.
    if history.status == 404 and index.status == 404:
        bootstrap = bootstrap_legacy(config.workspace_root, git=git)

    output_dir = config.output_dir / "benchmark"
    if config.benchmark_run_id:
        prepare_benchmark_update(config, output_dir, history, index, bootstrap)
    else:
        prepare_published_dashboard(output_dir, history, index, bootstrap)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Prepare the published CI performance dashboard"
    )
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--updates-dir", type=Path, default=None)
    parser.add_argument("--benchmark-run-id", default="")
    parser.add_argument("--benchmark-revision", default="")
    parser.add_argument("--benchmark-date", default="")
    parser.add_argument("--run-id", default=os.environ.get("GITHUB_RUN_ID", ""))
    parser.add_argument(
        "--run-attempt", default=os.environ.get("GITHUB_RUN_ATTEMPT", "")
    )
    parser.add_argument("--workspace-root", type=Path, default=WORKSPACE_ROOT)
    parser.add_argument("--title", default=DEFAULT_TITLE)
    parser.add_argument("--window", type=int, default=DEFAULT_WINDOW)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.benchmark_run_id and args.updates_dir is None:
        print(
            "::error::--updates-dir is required for benchmark dispatch mode",
            file=sys.stderr,
        )
        return 1
    config = PagesConfig(
        base_url=args.base_url,
        output_dir=args.output_dir,
        updates_dir=args.updates_dir if args.updates_dir is not None else Path(),
        benchmark_run_id=args.benchmark_run_id,
        benchmark_revision=args.benchmark_revision,
        benchmark_date=args.benchmark_date,
        run_id=args.run_id,
        run_attempt=args.run_attempt,
        workspace_root=args.workspace_root,
        title=args.title,
        window=args.window,
    )
    try:
        prepare_dashboard(config)
    except (PagesError, OSError, ValueError) as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
