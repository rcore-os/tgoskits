cmake_minimum_required(VERSION 3.20)
project(bug-ip-mtu-discover-udp-flush C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-ip-mtu-discover-udp-flush src/main.c)
target_compile_options(bug-ip-mtu-discover-udp-flush PRIVATE -Wall -Wextra -Werror)
target_link_options(bug-ip-mtu-discover-udp-flush PRIVATE -static)

install(TARGETS bug-ip-mtu-discover-udp-flush RUNTIME DESTINATION usr/bin/starry-test-suit)
