cmake_minimum_required(VERSION 3.20)

project(bugfix-epoll-rearm C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-epoll-rearm src/main.c)
target_include_directories(bugfix-epoll-rearm PRIVATE src)
target_compile_options(bugfix-epoll-rearm PRIVATE -Wall -Wextra -Werror)
target_link_libraries(bugfix-epoll-rearm PRIVATE pthread)

install(TARGETS bugfix-epoll-rearm RUNTIME DESTINATION usr/bin/starry-test-suit)
