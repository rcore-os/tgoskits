cmake_minimum_required(VERSION 3.20)
project(test-nix-builder-lifecycle C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-nix-builder-lifecycle src/main.c)
target_include_directories(test-nix-builder-lifecycle PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../../common)
target_compile_options(test-nix-builder-lifecycle PRIVATE -Wall -Wextra -Werror)
target_link_options(test-nix-builder-lifecycle PRIVATE -static)

install(TARGETS test-nix-builder-lifecycle RUNTIME DESTINATION usr/bin)
