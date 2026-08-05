cmake_minimum_required(VERSION 3.20)
project(test-mq-notify-unlink C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-mq-notify-unlink src/main.c)
target_include_directories(test-mq-notify-unlink PRIVATE src)
target_compile_options(test-mq-notify-unlink PRIVATE -Wall -Wextra -Werror)
target_link_options(test-mq-notify-unlink PRIVATE -static)

install(TARGETS test-mq-notify-unlink RUNTIME DESTINATION usr/bin/starry-test-suit)
