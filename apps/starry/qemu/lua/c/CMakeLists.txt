cmake_minimum_required(VERSION 3.20)

project(lua-app-test NONE)

install(PROGRAMS lua-app-tests.sh DESTINATION usr/bin)
install(FILES lua-main.lua lua-secondary.lua starry_lua_helper.lua DESTINATION usr/bin)
install(PROGRAMS ${STARRY_STAGING_ROOT}/usr/bin/lua5.4 DESTINATION usr/bin)
install(FILES ${STARRY_STAGING_ROOT}/usr/lib/lua/5.4/cjson.so DESTINATION usr/lib/lua/5.4)
