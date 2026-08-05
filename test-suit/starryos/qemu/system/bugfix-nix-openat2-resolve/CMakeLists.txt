cmake_minimum_required(VERSION 3.20)
project(bugfix-nix-openat2-resolve C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-nix-openat2-resolve src/main.c)
target_compile_options(bugfix-nix-openat2-resolve PRIVATE -Wall -Wextra -Werror)
install(TARGETS bugfix-nix-openat2-resolve RUNTIME DESTINATION usr/bin/starry-test-suit)
