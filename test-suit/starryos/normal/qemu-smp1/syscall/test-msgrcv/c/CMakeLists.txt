cmake_minimum_required(VERSION 3.20)
project(test-msgrcv C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-msgrcv src/main.c)
target_include_directories(test-msgrcv PRIVATE src)
target_compile_options(test-msgrcv PRIVATE -Wall -Wextra -Werror)

install(TARGETS test-msgrcv RUNTIME DESTINATION usr/bin/starry-test-suit)
