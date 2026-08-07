cmake_minimum_required(VERSION 3.20)
project(bugfix-mountinfo-poll-notify C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-mountinfo-poll-notify src/main.c)
target_compile_options(bugfix-mountinfo-poll-notify PRIVATE -Wall -Wextra -Werror)
install(
    TARGETS bugfix-mountinfo-poll-notify
    RUNTIME DESTINATION usr/bin/starry-test-suit
)
