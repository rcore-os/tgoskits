cmake_minimum_required(VERSION 3.20)
project(bug-epoll-wait-user-buffer-race C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-epoll-wait-user-buffer-race src/main.c)
target_compile_options(bug-epoll-wait-user-buffer-race PRIVATE -Wall -Wextra -Werror)
target_link_libraries(bug-epoll-wait-user-buffer-race PRIVATE pthread)

install(TARGETS bug-epoll-wait-user-buffer-race RUNTIME DESTINATION usr/bin/starry-test-suit)
