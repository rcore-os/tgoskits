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

luarocks-5.4 install /usr/share/lua-luarocks/inspect-3.1.3-0.rockspec || fail
test -f /usr/local/share/lua/5.4/inspect.lua || fail
inspect_show=$(luarocks-5.4 show inspect 2>/dev/null) || fail
printf '%s\n' "$inspect_show" | grep -F '3.1.3-0' >/dev/null || fail

lua5.4 /usr/bin/lua-luarocks-main.lua || fail
