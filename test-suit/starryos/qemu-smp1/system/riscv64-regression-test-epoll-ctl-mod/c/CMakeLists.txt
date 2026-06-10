cmake_minimum_required(VERSION 3.20)
project(test-epoll-ctl-mod C)
include(${CMAKE_CURRENT_LIST_DIR}/../../common/starry_arch_filter.cmake)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

starry_arch_filtered_executable(test-epoll-ctl-mod "riscv64" "test-epoll-ctl-mod skipped on unsupported architecture" src/main.c)
target_compile_options(test-epoll-ctl-mod PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-epoll-ctl-mod RUNTIME DESTINATION usr/bin/starry-test-suit)
