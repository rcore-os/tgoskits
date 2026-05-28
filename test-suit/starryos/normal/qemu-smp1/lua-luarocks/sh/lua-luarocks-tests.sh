#!/bin/sh
set -eu

fail() {
    echo "LUA_LUAROCKS_TEST_FAILED"
    exit 1
}

retry() {
    for _ in 1 2 3; do
        "$@" && return 0
        sleep 1
    done
    return 1
}

retry apk update || fail
retry apk add lua5.4 luarocks5.4 || fail

retry luarocks-5.4 install inspect || fail
test -f /usr/local/share/lua/5.4/inspect.lua || fail

lua5.4 /usr/bin/lua-luarocks-main.lua || fail
