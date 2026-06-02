#!/bin/sh

default_fetch_timeout=180
repo_file=/etc/apk/repositories
original_repos="$(cat "$repo_file")"

try_apk_curl() {
    mirror="$1"
    label="$2"
    fetch_timeout="$3"
    probe_url="$mirror/MIRRORS.txt"
    printf '%s\n' "$original_repos" |
        sed "s#http://[^/]*/alpine/#$mirror/#g;s#https://[^/]*/alpine/#$mirror/#g" > "$repo_file"
    rm -f /lib/apk/db/lock
    echo "APK_CURL_REPO_$label"
    echo "APK_CURL_UPDATE_BEGIN"
    timeout "$fetch_timeout" apk --timeout "$fetch_timeout" update &&
        echo "APK_CURL_UPDATE_DONE" &&
        echo "APK_CURL_ADD_BEGIN" &&
        timeout "$fetch_timeout" apk --timeout "$fetch_timeout" add curl &&
        echo "APK_CURL_ADD_DONE" &&
        echo "APK_CURL_PROBE_BEGIN" &&
        curl --version &&
        curl --connect-timeout 10 --max-time 30 -fsSL "$probe_url" -o /dev/null &&
        echo "APK_CURL_PROBE_DONE"
}

i=0
for repo in \
    "https://mirrors.cernet.edu.cn/alpine cernet 60" \
    "https://dl-cdn.alpinelinux.org/alpine upstream $default_fetch_timeout"
do
    i=$((i + 1))
    set -- $repo
    mirror=$1
    label=$2
    fetch_timeout=$3
    echo "APK_CURL_ATTEMPT_$i"
    if try_apk_curl "$mirror" "$label" "$fetch_timeout"; then
        echo 'APK_CURL_TEST_PASSED'
        exit 0
    fi
    sleep 2
done

echo 'APK_CURL_TEST_FAILED'
exit 1
