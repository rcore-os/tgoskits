cmake_minimum_required(VERSION 3.20)
project(test-clone-files-race-buddy-slab C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

set(TEST_CLONE_FILES_RACE_SRC
    "${CMAKE_CURRENT_LIST_DIR}/../../../qemu-smp4/test-clone-files-race/c/src/main.c")

add_executable(test-clone-files-race ${TEST_CLONE_FILES_RACE_SRC})
target_compile_options(test-clone-files-race PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-clone-files-race RUNTIME DESTINATION usr/bin)
