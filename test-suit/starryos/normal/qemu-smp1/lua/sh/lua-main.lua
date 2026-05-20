package.path = "/usr/bin/?.lua;" .. package.path

local helper = require("starry_lua_helper")
local cjson = require("cjson")

assert(_VERSION == "Lua 5.4", _VERSION)
assert(arg[1] == "alpha", "missing first script argument")
assert(arg[2] == "beta", "missing second script argument")

local joined = helper.join({ "starry", "lua", tostring(helper.answer()) }, "-")
assert(joined == "starry-lua-42", joined)

local encoded = cjson.encode({ runtime = "starry", values = { 1, 2, 3 } })
local decoded = cjson.decode(encoded)
assert(decoded.runtime == "starry", encoded)
assert(decoded.values[1] == 1 and decoded.values[2] == 2 and decoded.values[3] == 3, encoded)

local path = "/tmp/starry-lua-script-data.txt"
helper.write_lines(path, { "script", "module", joined })
local data = helper.read_all(path)
assert(data == "script\nmodule\nstarry-lua-42\n", data)

local dofile_value = dofile("/usr/bin/lua-secondary.lua")
assert(dofile_value == "secondary-ok", tostring(dofile_value))

print("LUA_APP_TEST_PASSED")
