"""Failure propagation for the conda notices smoke check."""

import os


# Conda 26.3.2's EnforceUnusedAdapter emits both lines for CONDA_OFFLINE=1.
CONDA_OFFLINE_ADAPTER_MARKER = "EnforceUnusedAdapter called with url"
CONDA_OFFLINE_DIAGNOSTIC = (
    "This command is using a remote connection in offline mode."
)
_UNEXPECTED_OUTPUT_MARKERS = ("Traceback", "INPROC ERROR:", "TIMEOUT:")
NOTICES_CHECK_LABEL = (
    "conda notices offline succeeds or reports the expected offline refusal"
)


def invoke_inproc(main, args, timeout_error=()):
    """Invoke conda's in-process entry point and normalize its completion."""
    try:
        code = main(*args)
    except timeout_error:
        raise
    except SystemExit as exc:
        code = exc.code
        if code is None:
            return 0, "", ""
        if not isinstance(code, int):
            return 1, "", ""
        return code, "", ""
    except Exception as exc:
        return 1, "", "INPROC ERROR: %s: %s" % (type(exc).__name__, exc)

    if isinstance(code, int):
        return int(code), "", ""
    return 0, "", ""


def run_offline(run, args, timeout=300):
    """Run one conda command with CONDA_OFFLINE=1 for that call only."""
    previous = os.environ.get("CONDA_OFFLINE")
    os.environ["CONDA_OFFLINE"] = "1"
    try:
        return run(args, timeout=timeout)
    finally:
        if previous is None:
            os.environ.pop("CONDA_OFFLINE", None)
        else:
            os.environ["CONDA_OFFLINE"] = previous


def notices_result_ok(rc, output):
    """Accept success or conda's explicit offline refusal, and nothing else."""
    if any(marker in output for marker in _UNEXPECTED_OUTPUT_MARKERS):
        return False
    offline_refusal = (
        CONDA_OFFLINE_ADAPTER_MARKER in output
        and CONDA_OFFLINE_DIAGNOSTIC in output
    )
    return rc == 0 or (rc == 1 and offline_refusal)


def verify_notices(run, check):
    """Run the notices check through the shared offline and failure rules."""
    rc, output = run_offline(run, ["notices"])
    passed = notices_result_ok(rc, output)
    check(passed, NOTICES_CHECK_LABEL)
    return passed
