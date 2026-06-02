cmake_minimum_required(VERSION 3.20)

project(apk-net-equivalence C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(apk-net-equivalence src/main.c)
target_compile_options(apk-net-equivalence PRIVATE -Wall -Wextra -Werror)

install(TARGETS apk-net-equivalence RUNTIME DESTINATION usr/bin)
