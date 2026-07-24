cmake_minimum_required(VERSION 3.20)
project(test-nix-namespace-exec C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-nix-namespace-exec src/main.c)
target_compile_options(test-nix-namespace-exec PRIVATE -Wall -Wextra -Werror)
target_link_options(test-nix-namespace-exec PRIVATE -static)

install(TARGETS test-nix-namespace-exec RUNTIME DESTINATION usr/bin/starry-test-suit)
