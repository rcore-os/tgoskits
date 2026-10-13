#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

cat > "$tmp_dir/curl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" > "$APACHE_RUNNER_TEST_LOG"
EOF
cat > "$tmp_dir/timeout" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" > "$APACHE_RUNNER_TIMEOUT_LOG"
exit 99
EOF
chmod +x "$tmp_dir/curl" "$tmp_dir/timeout"

PATH="$tmp_dir:$PATH"
export PATH
export APACHE_RUNNER_TEST_LOG="$tmp_dir/curl.log"
export APACHE_RUNNER_TIMEOUT_LOG="$tmp_dir/timeout.log"

. "$script_dir/apache-runner-lib.sh"
APACHE_RUNNER_TIMEOUT_CMD=timeout

apache_runner_run_with_timeout 5 curl -fsS http://127.0.0.1:8080/
[ "$(cat "$APACHE_RUNNER_TEST_LOG")" = '--max-time 5 -fsS http://127.0.0.1:8080/' ]
[ ! -e "$APACHE_RUNNER_TIMEOUT_LOG" ]

if apache_runner_run_with_timeout 5 sh -c true; then
    printf 'expected the generic timeout wrapper to run\n' >&2
    exit 1
fi
[ "$(cat "$APACHE_RUNNER_TIMEOUT_LOG")" = '5 sh -c true' ]

printf 'APACHE_RUNNER_LIB_TESTS_PASSED\n'
