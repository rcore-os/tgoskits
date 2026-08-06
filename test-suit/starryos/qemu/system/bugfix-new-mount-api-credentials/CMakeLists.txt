cmake_minimum_required(VERSION 3.20)
project(bugfix-new-mount-api-credentials C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-new-mount-api-credentials src/main.c)
target_compile_options(bugfix-new-mount-api-credentials PRIVATE -Wall -Wextra -Werror)
install(TARGETS bugfix-new-mount-api-credentials RUNTIME DESTINATION usr/bin/starry-test-suit)
