cmake_minimum_required(VERSION 3.20)

project(bug-new-mount-api-enosys C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-new-mount-api-enosys src/main.c)
target_compile_options(bug-new-mount-api-enosys PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-new-mount-api-enosys RUNTIME DESTINATION usr/bin/starry-test-suit)
