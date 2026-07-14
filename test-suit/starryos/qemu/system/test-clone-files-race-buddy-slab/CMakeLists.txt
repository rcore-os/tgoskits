cmake_minimum_required(VERSION 3.20)
project(test-clone-files-race-buddy-slab C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

set(TEST_CLONE_FILES_RACE_SRC
    "${CMAKE_CURRENT_LIST_DIR}/../test-clone-files-race/src/main.c")

include("${CMAKE_CURRENT_LIST_DIR}/../common/starry_arch_filter.cmake")
starry_arch_filtered_executable(
    test-clone-files-race-buddy-slab
    "aarch64"
    "test-clone-files-race-buddy-slab skipped on non-aarch64 qemu"
    ${TEST_CLONE_FILES_RACE_SRC})
target_compile_options(test-clone-files-race-buddy-slab PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-clone-files-race-buddy-slab RUNTIME DESTINATION usr/bin/starry-test-suit)
