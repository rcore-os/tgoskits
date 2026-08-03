cmake_minimum_required(VERSION 3.20)
project(test-nixpkgs-first-divergence C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-nixpkgs-first-divergence src/main.c)
target_compile_options(test-nixpkgs-first-divergence PRIVATE -Wall -Wextra -Werror)
target_link_options(test-nixpkgs-first-divergence PRIVATE -static)

install(TARGETS test-nixpkgs-first-divergence RUNTIME DESTINATION usr/bin/starry-test-suit)
