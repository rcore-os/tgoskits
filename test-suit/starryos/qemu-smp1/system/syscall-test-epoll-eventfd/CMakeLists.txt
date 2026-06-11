cmake_minimum_required(VERSION 3.20)
project(test-epoll-eventfd C)
include(${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
starry_arch_filtered_executable(test-epoll-eventfd "x86_64" "test-epoll-eventfd skipped on unsupported architecture" src/main.c)
target_include_directories(test-epoll-eventfd PRIVATE src)
target_compile_options(test-epoll-eventfd PRIVATE -Wall -Wextra -Werror)
# Build this stricter probe, but keep it out of the grouped CI pass condition
# until Starry's EPOLLET eventfd behavior matches Linux.
install(TARGETS test-epoll-eventfd RUNTIME DESTINATION usr/bin/starry-known-fail)
