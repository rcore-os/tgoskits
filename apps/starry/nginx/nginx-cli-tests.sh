#!/bin/sh
set -eu

cmd=${1:-all}

case "$cmd" in
    smoke)
        sh ./smoke/nginx-smoke-tests.sh
        ;;
    phase1)
        sh ./phase/nginx-1-3-lifecycle-tests.sh
        ;;
    phase2)
        sh ./phase/nginx-2-0-http-basic-tests.sh
        ;;
    all)
        sh ./smoke/nginx-smoke-tests.sh
        sh ./phase/nginx-1-3-lifecycle-tests.sh
        sh ./phase/nginx-2-0-http-basic-tests.sh
        printf 'NGINX_CLI_ALL_PASSED\n'
        ;;
    *)
        printf 'usage: %s [smoke|phase1|phase2|all]\n' "$0"
        exit 2
        ;;
esac
