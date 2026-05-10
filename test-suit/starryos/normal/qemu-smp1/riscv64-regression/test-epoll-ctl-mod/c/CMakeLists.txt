cmake_minimum_required(VERSION 3.20)
project(test-epoll-ctl-mod C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-epoll-ctl-mod src/main.c)
target_compile_options(test-epoll-ctl-mod PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-epoll-ctl-mod RUNTIME DESTINATION usr/bin)
