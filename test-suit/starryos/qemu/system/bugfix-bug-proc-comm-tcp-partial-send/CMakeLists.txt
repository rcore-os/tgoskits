cmake_minimum_required(VERSION 3.20)
project(bug-proc-comm-tcp-partial-send C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-proc-comm-tcp-partial-send src/main.c)
target_include_directories(bug-proc-comm-tcp-partial-send PRIVATE src)
target_compile_options(bug-proc-comm-tcp-partial-send PRIVATE -Wall -Wextra -Werror)
target_link_options(bug-proc-comm-tcp-partial-send PRIVATE -static)

install(TARGETS bug-proc-comm-tcp-partial-send RUNTIME DESTINATION usr/bin/starry-test-suit)
