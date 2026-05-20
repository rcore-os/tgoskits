#!/bin/sh
set -eu

apk update
apk add lua5.4 lua5.4-cjson

lua5.4 /usr/bin/lua-main.lua alpha beta || {
    echo "LUA_APP_TEST_FAILED"
    exit 1
}
