#!/bin/sh
# apache-tcp-defer-accept-probe.sh
#
# Runs the static setsockopt(TCP_DEFER_ACCEPT) probe and maps its result onto
# the runner PASS/FAIL markers so it can be driven by a qemu debug config.
#
# This isolates the StarryOS syscall behaviour from Apache: it does not depend
# on which apache2/APR build the Alpine mirror happens to serve.

PROBE=/usr/bin/tcp-defer-accept-probe

printf 'APACHE_RUNNER_PHASE_BEGIN phase=debug:tcp-defer-accept\n'

if [ ! -x "$PROBE" ]; then
    printf 'APACHE_APP_LOG: probe binary missing: %s\n' "$PROBE"
    printf 'APACHE_RUNNER_FAILED phase=debug:tcp-defer-accept rc=127\n'
    exit 1
fi

"$PROBE"
rc=$?

if [ "$rc" -eq 0 ]; then
    printf 'APACHE_RUNNER_PASSED mode=debug:tcp-defer-accept\n'
    exit 0
fi

printf 'APACHE_RUNNER_FAILED phase=debug:tcp-defer-accept rc=%s\n' "$rc"
exit 1
