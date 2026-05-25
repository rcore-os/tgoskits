cmake_minimum_required(VERSION 3.20)
project(test-gdb-native-smoke C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(gdb-native-smoke-target src/main.c)
target_compile_options(gdb-native-smoke-target PRIVATE -Wall -Wextra -Werror -O0 -g)

install(TARGETS gdb-native-smoke-target RUNTIME DESTINATION usr/bin)
install(PROGRAMS gdb-native-smoke.gdb DESTINATION usr/bin)

if(DEFINED STARRY_STAGING_ROOT)
    install(PROGRAMS "${STARRY_STAGING_ROOT}/usr/bin/gdb" DESTINATION usr/bin)
    if(EXISTS "${STARRY_STAGING_ROOT}/usr/share/gdb")
        install(DIRECTORY "${STARRY_STAGING_ROOT}/usr/share/gdb" DESTINATION usr/share)
    endif()
    if(EXISTS "${STARRY_STAGING_ROOT}/usr/lib/python3.12")
        install(DIRECTORY "${STARRY_STAGING_ROOT}/usr/lib/python3.12" DESTINATION usr/lib)
    endif()
endif()
