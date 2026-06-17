#!/bin/sh
set -eu

cmd=${1:-all}

case "$cmd" in
    smoke)
        sh ./smoke/nginx-smoke-tests.sh
        ;;
    phase00)
        sh ./phase/nginx-0-0-env-rlimit-tests.sh
        ;;
    phase12)
        sh ./phase/nginx-1-2-lifecycle-tests.sh
        ;;
    phase13)
        sh ./phase/nginx-1-3-lifecycle-tests.sh
        ;;
    phase20)
        sh ./phase/nginx-2-0-http-basic-tests.sh
        ;;
    phase31)
        sh ./phase/nginx-3-1-short-connection-tests.sh
        ;;
    phase32)
        sh ./phase/nginx-3-2-keepalive-tests.sh
        ;;
    phase33)
        sh ./phase/nginx-3-3-slow-header-tests.sh
        ;;
    phase41)
        sh ./phase/nginx-4-1-sendfile-off-tests.sh
        ;;
    phase42)
        sh ./phase/nginx-4-2-sendfile-on-tests.sh
        ;;
    phase43)
        sh ./phase/nginx-4-3-range-tests.sh
        ;;
    phase50)
        sh ./phase/nginx-5-0-request-body-tests.sh
        ;;
    phase60)
        sh ./phase/nginx-6-0-log-fs-tests.sh
        ;;
    phase70)
        sh ./phase/nginx-7-0-signal-lifecycle-tests.sh
        ;;
    phase90)
        sh ./phase/nginx-9-0-config-feature-tests.sh
        ;;
    all)
        sh ./smoke/nginx-smoke-tests.sh
        sh ./phase/nginx-0-0-env-rlimit-tests.sh
        sh ./phase/nginx-1-2-lifecycle-tests.sh
        sh ./phase/nginx-1-3-lifecycle-tests.sh
        sh ./phase/nginx-2-0-http-basic-tests.sh
        sh ./phase/nginx-3-1-short-connection-tests.sh
        sh ./phase/nginx-3-2-keepalive-tests.sh
        sh ./phase/nginx-3-3-slow-header-tests.sh
        sh ./phase/nginx-4-1-sendfile-off-tests.sh
        sh ./phase/nginx-4-2-sendfile-on-tests.sh
        sh ./phase/nginx-4-3-range-tests.sh
        sh ./phase/nginx-5-0-request-body-tests.sh
        sh ./phase/nginx-6-0-log-fs-tests.sh
        sh ./phase/nginx-7-0-signal-lifecycle-tests.sh
        sh ./phase/nginx-9-0-config-feature-tests.sh
        printf 'NGINX_CLI_ALL_PASSED\n'
        ;;
    *)
        printf 'usage: %s [smoke|phase00|phase12|phase13|phase20|phase31|phase32|phase33|phase41|phase42|phase43|phase50|phase60|phase70|phase90|all]\n' "$0"
        exit 2
        ;;
esac
