cmake_minimum_required(VERSION 3.20)
project(test-netlink-recvmsg C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-netlink-recvmsg src/main.c)
target_compile_options(test-netlink-recvmsg PRIVATE -Wall -Wextra)
target_link_options(test-netlink-recvmsg PRIVATE -static)

install(TARGETS test-netlink-recvmsg RUNTIME DESTINATION usr/bin/starry-test-suit)
