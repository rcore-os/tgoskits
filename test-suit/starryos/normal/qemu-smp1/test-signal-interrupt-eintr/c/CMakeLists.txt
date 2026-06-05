cmake_minimum_required(VERSION 3.20)
project(test-signal-interrupt-eintr C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-signal-interrupt-eintr src/main.c)
target_include_directories(test-signal-interrupt-eintr PRIVATE src)
target_compile_options(test-signal-interrupt-eintr PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-signal-interrupt-eintr RUNTIME DESTINATION usr/bin)
