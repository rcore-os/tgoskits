cmake_minimum_required(VERSION 3.20)

project(test-timer-family C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-timer-family src/main.c)
target_compile_options(test-timer-family PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-timer-family PRIVATE rt pthread)

install(TARGETS test-timer-family RUNTIME DESTINATION usr/bin)
