#!/bin/sh
set -eu

apk update
apk add lua5.4 luarocks5.4

luarocks-5.4 install inspect
test -f /usr/local/share/lua/5.4/inspect.lua

lua5.4 /usr/bin/lua-luarocks-main.lua || {
    echo "LUA_LUAROCKS_TEST_FAILED"
    exit 1
}
