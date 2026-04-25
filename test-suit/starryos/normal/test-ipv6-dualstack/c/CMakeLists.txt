cmake_minimum_required(VERSION 3.20)
project(test-ipv6-dualstack C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-ipv6-dualstack src/main.c)
target_include_directories(test-ipv6-dualstack PRIVATE src)
target_compile_options(test-ipv6-dualstack PRIVATE -Wall -Wextra)

install(TARGETS test-ipv6-dualstack RUNTIME DESTINATION usr/bin)
