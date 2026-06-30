#!/bin/sh

default_fetch_timeout=300
repo_file=/etc/apk/repositories
original_repos="$(cat "$repo_file")"

try_install_deps() {
    mirror="$1"
    label="$2"
    fetch_timeout="$3"

    printf '%s\n' "$original_repos" |
        sed "s#http://[^/]*/alpine/#$mirror/#g;s#https://[^/]*/alpine/#$mirror/#g" > "$repo_file"
    rm -f /lib/apk/db/lock

    echo "OPENSSL_LA_REPO_$label"
    echo "OPENSSL_LA_UPDATE_BEGIN"
    timeout "$fetch_timeout" apk --timeout "$fetch_timeout" update &&
        echo "OPENSSL_LA_UPDATE_DONE" &&
        echo "OPENSSL_LA_ADD_BEGIN" &&
        timeout "$fetch_timeout" apk --timeout "$fetch_timeout" add python3 openssl &&
        echo "OPENSSL_LA_ADD_DONE"
}

i=0
for repo in \
    "https://mirrors.cernet.edu.cn/alpine cernet $default_fetch_timeout" \
    "https://dl-cdn.alpinelinux.org/alpine upstream $default_fetch_timeout" \
    "http://mirrors.tuna.tsinghua.edu.cn/alpine tuna $default_fetch_timeout"
do
    i=$((i + 1))
    set -- $repo
    mirror=$1
    label=$2
    fetch_timeout=$3
    echo "OPENSSL_LA_ATTEMPT_$i"
    if try_install_deps "$mirror" "$label" "$fetch_timeout"; then
        exec /usr/bin/probe-openssl-loongarch.sh
    fi
    sleep 2
done

echo "OPENSSL_LA_HAS_FAILURES"
exit 1
