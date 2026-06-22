cmake_minimum_required(VERSION 3.20)
project(test-si-addr-sigsegv C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-si-addr-sigsegv src/main.c)
target_compile_options(test-si-addr-sigsegv PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-si-addr-sigsegv RUNTIME DESTINATION usr/bin/starry-test-suit)
