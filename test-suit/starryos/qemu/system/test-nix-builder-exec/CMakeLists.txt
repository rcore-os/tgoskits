cmake_minimum_required(VERSION 3.20)
project(test-nix-builder-exec C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-nix-builder-exec src/main.c)
target_compile_options(test-nix-builder-exec PRIVATE -Wall -Wextra -Werror)
target_link_options(test-nix-builder-exec PRIVATE -static)

install(TARGETS test-nix-builder-exec RUNTIME DESTINATION usr/bin/starry-test-suit)
