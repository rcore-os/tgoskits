#!/bin/sh
set -eu

cmd=${1:-smoke}

case "$cmd" in
    smoke)
        sh ./smoke/apache-smoke-tests.sh
        ;;
    phase20)
        sh ./phase/apache-2-0-mpm-prefork-tests.sh
        ;;
    phase30)
        sh ./phase/apache-3-0-http-static-tests.sh
        ;;
    phase40)
        sh ./phase/apache-4-0-directory-access-tests.sh
        ;;
    phase50)
        sh ./phase/apache-5-0-log-lifecycle-tests.sh
        ;;
    phase55)
        sh ./phase/apache-5-5-sendfile-range-tests.sh
        ;;
    phase70)
        sh ./phase/apache-7-0-cgi-tests.sh
        ;;
    phase80)
        sh ./phase/apache-8-0-module-feature-tests.sh
        ;;
    all)
        sh ./smoke/apache-smoke-tests.sh
        sh ./phase/apache-2-0-mpm-prefork-tests.sh
        sh ./phase/apache-3-0-http-static-tests.sh
        sh ./phase/apache-4-0-directory-access-tests.sh
        sh ./phase/apache-5-0-log-lifecycle-tests.sh
        sh ./phase/apache-5-5-sendfile-range-tests.sh
        sh ./phase/apache-7-0-cgi-tests.sh
        sh ./phase/apache-8-0-module-feature-tests.sh
        printf 'APACHE_CLI_ALL_PASSED\n'
        ;;
    *)
        printf 'usage: %s [smoke|phase20|phase30|phase40|phase50|phase55|phase70|phase80|all]\n' "$0"
        exit 2
        ;;
esac
