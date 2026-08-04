cmake_minimum_required(VERSION 3.20)
project(test-loongarch-unaligned-cross-page C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-loongarch-unaligned-cross-page src/main.c)
target_compile_options(test-loongarch-unaligned-cross-page PRIVATE -Wall -Wextra -Werror)

install(
    TARGETS test-loongarch-unaligned-cross-page
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
