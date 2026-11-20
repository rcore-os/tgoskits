cmake_minimum_required(VERSION 3.20)

project(tcp-half-close-reply C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(tcp-half-close-reply src/main.c)
target_compile_options(tcp-half-close-reply PRIVATE -Wall -Wextra -Werror)
target_link_libraries(tcp-half-close-reply PRIVATE pthread)

install(TARGETS tcp-half-close-reply RUNTIME DESTINATION usr/bin/starry-test-suit)
