cmake_minimum_required(VERSION 3.20)
project(test-nix-clone-parent C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

include("${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake")
starry_arch_filtered_executable(
    test-nix-clone-parent
    "^x86_64"
    "test-nix-clone-parent skipped on non-x86_64 qemu"
    src/main.c)
target_compile_options(test-nix-clone-parent PRIVATE -Wall -Wextra -Werror)
target_link_options(test-nix-clone-parent PRIVATE -static)

install(TARGETS test-nix-clone-parent RUNTIME DESTINATION usr/bin/starry-test-suit)
