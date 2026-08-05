cmake_minimum_required(VERSION 3.20)

project(bugfix-signalfd-epoll-wakeup C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-signalfd-epoll-wakeup src/main.c)
target_compile_options(bugfix-signalfd-epoll-wakeup PRIVATE -Wall -Wextra -Werror)
target_link_libraries(bugfix-signalfd-epoll-wakeup PRIVATE pthread)

install(TARGETS bugfix-signalfd-epoll-wakeup RUNTIME DESTINATION usr/bin/starry-test-suit)
