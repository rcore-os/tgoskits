cmake_minimum_required(VERSION 3.20)
project(test-mount-namespace-isolation C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-mount-namespace-isolation src/main.c)
target_include_directories(test-mount-namespace-isolation PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../../common)
target_compile_options(test-mount-namespace-isolation PRIVATE -Wall -Wextra -Werror)
target_link_options(test-mount-namespace-isolation PRIVATE -static)

install(TARGETS test-mount-namespace-isolation RUNTIME DESTINATION usr/bin/starry-test-suit)
