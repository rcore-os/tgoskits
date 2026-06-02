local inspect = require("inspect")

local rendered = inspect({ runtime = "starry", values = { 1, 2, 3 } })
assert(rendered:match('runtime = "starry"'), rendered)
assert(rendered:match("values"), rendered)

print("LUA_LUAROCKS_TEST_PASSED")
