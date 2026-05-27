#!/bin/sh
set -eu

lua5.4 /usr/bin/lua-main.lua alpha beta || {
    echo "LUA_APP_TEST_FAILED"
    exit 1
}
