cmake_minimum_required(VERSION 3.20)
project(test-nix-clone-parent C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-nix-clone-parent src/main.c)
target_compile_options(test-nix-clone-parent PRIVATE -Wall -Wextra -Werror)
target_link_options(test-nix-clone-parent PRIVATE -static)

install(TARGETS test-nix-clone-parent RUNTIME DESTINATION usr/bin/starry-test-suit)
