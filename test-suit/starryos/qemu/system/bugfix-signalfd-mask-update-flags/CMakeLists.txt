cmake_minimum_required(VERSION 3.20)

project(bugfix-signalfd-mask-update-flags C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-signalfd-mask-update-flags src/main.c)
target_compile_options(bugfix-signalfd-mask-update-flags PRIVATE -Wall -Wextra -Werror)

install(TARGETS bugfix-signalfd-mask-update-flags RUNTIME DESTINATION usr/bin/starry-test-suit)
