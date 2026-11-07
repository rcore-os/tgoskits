"""Shared conda invocation, environment scoping, JSON extraction, and diagnostics."""

import contextlib
import json
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


def invoke_inproc(
    main,
    args,
    timeout_error=(),
    timeout_state=None,
    timeout_message=None,
):
    """Invoke conda's in-process entry point and normalize its completion.

    ``timeout_state.expired`` is set by the caller's signal handler. Conda's own
    exception handler catches broad exception types, so this helper is the one
    place that maps a swallowed timeout back to its caller-supplied diagnostic.
    """
    extra_out = extra_err = ""
    try:
        code = main(*args)
    except timeout_error:
        raise
    except SystemExit as exc:
        code = exc.code
        if code is None:
            code = 0
        elif not isinstance(code, int):
            code = 1
    except Exception as exc:
        code, extra_out, extra_err = (
            1,
            "",
            "INPROC ERROR: %s: %s" % (type(exc).__name__, exc),
        )
    else:
        code = int(code) if isinstance(code, int) else 0

    if timeout_state is not None and getattr(timeout_state, "expired", False):
        return (
            124,
            "",
            timeout_message or "TIMEOUT: in-process command exceeded its time limit",
        )

    return int(code), extra_out, extra_err


def loadjson(text):
    """Extract one JSON document while tolerating text before or after it."""
    if not text:
        return None
    text = text.strip()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass

    decoder = json.JSONDecoder()
    for index, char in enumerate(text):
        if char not in "{[":
            continue
        try:
            document, _ = decoder.raw_decode(text[index:])
            return document
        except json.JSONDecodeError:
            continue
    return None


_DIAGNOSTIC_JSON_KEYS = (
    "exception_name",
    "exception_type",
    "error",
    "message",
    "success",
    "command",
    "traceback",
)


def _first_line(text, limit):
    for line in (text or "").splitlines():
        line = " ".join(line.split())
        if line:
            return line[:limit]
    return "<no output>"


def _json_failure(text, limit=2000):
    document = loadjson(text)
    if not isinstance(document, dict):
        return None
    if not any(key in document for key in ("exception_name", "error", "traceback")):
        return None

    fields = {}
    for key in _DIAGNOSTIC_JSON_KEYS:
        if key not in document:
            continue
        value = document[key]
        if isinstance(value, str) and len(value) > limit:
            value = "<truncated>..." + value[-limit:]
        fields[key] = value
    return json.dumps(fields, ensure_ascii=False, sort_keys=True)


def describe_output(rc, stdout, stderr, limit=140):
    """Keep successful output compact and expose locator fields on failure."""
    stdout_detail = _json_failure(stdout) if rc != 0 else None
    stderr_detail = _json_failure(stderr) if rc != 0 else None
    return "rc=%s stdout=%r stderr=%r" % (
        rc,
        stdout_detail or _first_line(stdout, limit),
        stderr_detail or _first_line(stderr, limit),
    )


@contextlib.contextmanager
def temporary_env(name, value):
    """Set one environment variable for a call and restore it on every exit."""
    previous = os.environ.get(name)
    os.environ[name] = value
    try:
        yield
    finally:
        if previous is None:
            os.environ.pop(name, None)
        else:
            os.environ[name] = previous


def run_offline(run, args, timeout=300):
    """Run one conda command with CONDA_OFFLINE=1 for that call only."""
    with temporary_env("CONDA_OFFLINE", "1"):
        return run(args, timeout=timeout)


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
